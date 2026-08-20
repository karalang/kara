//! `karac build` / `build-project` / `doc` — AOT artifact production.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 3).

use super::*;
use rustc_hash::FxHashMap;

#[allow(clippy::too_many_arguments)]
pub(super) fn cmd_build(
    filename: &str,
    output: OutputMode,
    concurrency_report: bool,
    simd_report: bool,
    offline: bool,
    enable_hot_swap: bool,
    no_proxy: bool,
    target: Option<&str>,
    bindings: Option<BindingsMode>,
    target_cpu: Option<&str>,
    target_features: Option<&str>,
    wasm_threads: bool,
    monomorphization_budget: crate::monomorphization::MonomorphizationBudget,
    release: bool,
    crate_type: NativeCrateType,
    out_path: Option<&str>,
    lint_overrides: crate::lints::CliLintOverrides,
) {
    // Single-file mode runs no dep resolution and reaches no network surface,
    // so `--offline` is silently accepted for ergonomic CLI consistency with
    // project mode (operators script both via the same flag set).
    let _ = offline;
    // Phase-10 WASM build path: a `--target` value from the closed v1 name
    // set (`native`, `wasm_browser`, `wasm_wasi`, `gpu`) selects the
    // compilation target — it swaps the process-wide active target that
    // `filter_inactive_items` (`#[target(...)]` absence semantics), the
    // resolver's tombstone diagnostics, and the effect checker's E0411
    // target gate all read. Any other value is a rustc-style triple, which
    // only project mode consumes (manifest `[target.<triple>.*]` overlay
    // merge) and stays accepted-but-inert in single-file mode.
    let build_target = resolve_build_target(target);
    // Single-file `karac build` on a file that is a member of a `kara.toml`
    // PACKAGE (lives under the package's `src/` directory) silently drops the
    // sibling modules it `import`s and emits a truncated binary that links but
    // does nothing — an unresolvable local-module import is accepted rather
    // than erroring in single-file mode (B-2026-07-08-19). `karac run` on the
    // same file auto-discovers the package and works, so this is a build-only
    // footgun. Refuse it with actionable guidance instead of producing junk;
    // gated tightly (file under `<root>/src/`) so a genuinely standalone script
    // that merely sits near a manifest is unaffected.
    if let Some(msg) = package_member_build_refusal(filename) {
        emit_build_error(&msg, output);
        process::exit(1);
    }
    emit_no_proxy_note(no_proxy);
    let _ = no_proxy;
    #[cfg(feature = "llvm")]
    {
        // CPU baseline override (phase-10 `--target-cpu`; design.md §
        // CPU Baseline Targeting). Precedence: CLI flag >
        // `KARAC_TARGET_CPU` env > the discovered manifest's
        // `[release] target-cpu` (walk-up from the file's directory —
        // the `karac run` discovery rule; only consulted when both
        // higher tiers are absent, so an explicit flag/env build never
        // gains a manifest-error failure mode). Runs after
        // `resolve_build_target` — `help` and validation are
        // per-active-target — and before any pipeline pass, failing
        // fast on a typo'd name.
        // Arch-portable `cpu-baseline` (walk-up-discovered manifest), with the
        // design's `v3` default applied when no explicit level — the LOWEST tier
        // of both chains. Same resolution as the project-build path.
        let (baseline_cpu, baseline_features) = resolve_native_cpu_baseline(
            manifest_release_field_for(filename, output, |m| m.release_cpu_baseline.clone())
                .as_deref(),
        );
        apply_target_cpu_override(
            target_cpu
                .map(str::to_string)
                .or_else(read_target_cpu_env)
                .or_else(|| {
                    manifest_release_field_for(filename, output, |m| m.release_target_cpu.clone())
                })
                .or(baseline_cpu),
        );
        // Feature-string override — the sibling chain, resolved
        // independently (a flag-supplied CPU does not suppress a
        // manifest-supplied feature list, and vice versa).
        apply_target_features_override(
            target_features
                .map(str::to_string)
                .or_else(|| {
                    read_target_features_env().or_else(|| {
                        manifest_release_field_for(filename, output, |m| {
                            m.release_target_features.clone()
                        })
                    })
                })
                .or(baseline_features),
        );
        let is_wasm = build_target == "wasm_wasi" || build_target == "wasm_browser";
        // Library-artifact producer mode (additive-interop Slice 2;
        // design.md § Exported C ABI) is native-only. A wasm build already
        // has its own producer surface — module exports selected by
        // `--bindings` (`crate::wasm_exports`) — so a `--crate-type
        // staticlib/cdylib` there is a category error, not a silent no-op.
        // Reject before any pipeline work (the `--target-cpu` fail-fast
        // posture).
        if is_wasm && crate_type != NativeCrateType::Bin {
            eprintln!(
                "error: --crate-type staticlib/cdylib is a native-only producer mode; \
                 for a wasm library surface use `--target={build_target}` with `--bindings` \
                 (the module-export path). See design.md § Exported C ABI."
            );
            process::exit(1);
        }
        // External native-library linking (`kara.toml` `[link]` table) —
        // native targets only (wasm-ld ignores it). Manifest-only, no
        // CLI/env tier; discovered by the same walk-up as the CPU/features
        // chains. Set before codegen so the linker invocation sees it.
        if !is_wasm {
            let (link_libs, link_search_paths) = manifest_link_config_for(filename, output);
            apply_native_link_config(link_libs, link_search_paths);
        }
        let effective_bindings = resolve_effective_bindings(build_target, bindings);
        // Hot-swap requires dynamic symbol resolution at runtime; a wasm
        // module has none. Same gate as project mode (the wasm half of
        // the phase-7 hot-swap target gating).
        if enable_hot_swap && is_wasm {
            eprintln!(
                "error: --enable-hot-swap is incompatible with --target={build_target} \
                 (no dynamic-symbol-resolution machinery on wasm hosts)"
            );
            process::exit(1);
        }
        // `--features wasm-threads` scope gate (phase-10 wasm-threads
        // entry). The flag is wasm_browser-only: wasi-threads (the
        // preview1-era host-threading ABI the threaded substrate builds
        // on) and the component model don't compose, and wasm_wasi's
        // default bindings are component — host-thread integration for
        // wasm_wasi is a design.md § WASM Concurrency Lowering future
        // concern, not a v1 surface. Same reasoning rejects an explicit
        // `--bindings=component` on a wasm_browser threaded build.
        validate_wasm_threads_scope(wasm_threads, build_target, effective_bindings);
        // Phase-10 WASM entry-point discovery: browser + component
        // bindings marshal rich exports (canonical-ABI trampolines);
        // `--bindings none` keeps raw core exports. Signal codegen before
        // it runs.
        crate::target::set_wasm_export_marshalling(matches!(
            effective_bindings,
            Some(BindingsMode::Browser) | Some(BindingsMode::Component)
        ));
        // Derive the output stem early — embedded component bindings
        // need it as the WIT package name before codegen runs.
        let exe_name = std::path::Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        // Embedded-WIT component bindings (phase-10 "embedded-WIT
        // migration"): resolve the external componentization tool up
        // front — a missing or mis-pinned wasm-tools fails before any
        // pipeline work, the `--target-cpu` fail-fast posture — and
        // install the package name that flips codegen's host-fn import
        // attachment to canonical-ABI `kara:<pkg>/host` naming. The
        // pin rides the same lazy manifest walk-up as the `[release]`
        // chain (`[toolchain] wasm-tools`).
        let wasm_tools = match effective_bindings {
            Some(BindingsMode::Component) => {
                let pin = manifest_release_field_for(filename, output, |m| {
                    m.toolchain_wasm_tools.clone()
                });
                match crate::componentize::resolve_wasm_tools(pin.as_deref()) {
                    Ok(tool) => {
                        crate::target::set_wasm_component_host_package(exe_name);
                        Some(tool)
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        process::exit(1);
                    }
                }
            }
            _ => None,
        };
        let source = read_source(filename);
        let mut pipeline = Pipeline::new(filename, &source).with_lint_overrides(lint_overrides);
        pipeline.resolve();
        pipeline.typecheck();
        pipeline.lower();
        pipeline.effectcheck();
        pipeline.ownershipcheck();
        // Auto-par codegen (slice 2): populate `pipeline.concurrency` so the
        // codegen call below picks up inferred parallel groups via
        // `Codegen::parallel_groups_for_current_fn`. `concurrencycheck` is a
        // no-op when `effectcheck` produced no result (`self.effects.is_none()`),
        // so phase ordering follows effects → ownership → concurrency.
        pipeline.concurrencycheck();

        // Slice D: emit the human-readable concurrency report before the
        // codegen / link stage so it lands on stdout next to the
        // `Built: <exe>` line, regardless of whether codegen later fails.
        if concurrency_report {
            emit_concurrency_report(&pipeline);
        }
        // SIMD lowering report (slice 5b) — same pre-codegen placement, so it
        // prints even when a `#[require_simd]` violation later aborts the build.
        if simd_report {
            emit_simd_report(&pipeline);
        }

        if pipeline.has_fatal_errors() {
            match output {
                OutputMode::Text => {
                    print_text_diagnostics(&pipeline);
                    process::exit(1);
                }
                OutputMode::Json => {
                    emit_json_output(&pipeline);
                    process::exit(1);
                }
                OutputMode::Jsonl => unreachable!(),
            }
        }

        // B-2026-08-18-1 — the SUCCESS path's warnings. Everything above only
        // renders when the build is about to fail, so a build that produced a
        // binary printed nothing but `Built: …` no matter how many warnings the
        // compiler had computed. Measured on `#[deprecated]`: `karac check`
        // reports it, `karac build` did not.
        //
        // Read out of the same renderer `check` uses, so the two lanes cannot
        // word a warning differently. Text goes to stderr here; the JSON lane
        // carries them on the success object instead (see `build_warnings_json`
        // at the emit site), because a build's JSON is a single object and a
        // second one would break every consumer that reads one.
        let build_warnings_json = match output {
            OutputMode::Text => {
                for block in render_text_warning_diagnostics(&pipeline) {
                    eprintln!("{block}");
                }
                Vec::new()
            }
            OutputMode::Json => collect_warning_diagnostics_json(&pipeline),
            OutputMode::Jsonl => Vec::new(),
        };

        // `#[require_simd]` guarantee (phase-7-codegen.md line 308, slice 5a):
        // a function annotated `#[require_simd]` must not contain any
        // `Vector[T, N]` op that would scalarize on the target. Checked after
        // a clean typecheck, before codegen — the analysis consumes the
        // `expr_types` side-table the typechecker populated. Aborts the build
        // (the `check` path surfaces the same diagnostics non-fatally through
        // `simd_check` in `run_all_checks`). Print only the SIMD diagnostics
        // here — effect/ownership/concurrency findings are non-fatal at this
        // build stage and are intentionally not surfaced by this abort.
        pipeline.simd_check();
        let simd_errors = pipeline.simd_errors.clone().unwrap_or_default();
        if !simd_errors.is_empty() {
            match output {
                OutputMode::Json => emit_json_output(&pipeline),
                OutputMode::Text | OutputMode::Jsonl => {
                    for e in &simd_errors {
                        eprintln!(
                            "error[E_REQUIRE_SIMD]: {}:{}:{} (in `{}`): {}",
                            filename,
                            e.span.line,
                            e.span.column,
                            e.func_name,
                            e.message(),
                        );
                        eprintln!("  help: {}", e.help());
                    }
                }
            }
            process::exit(1);
        }

        // Monomorphization budget (v1.x): per-generic instantiation
        // ceiling enforced after a clean typecheck, before codegen. A
        // disabled budget is a no-op; an error-level violation fails the
        // build here (sparing codegen work), warn-level emits a note and
        // continues. See phase-7-codegen.md line 266.
        if monomorphization_budget.is_enabled() {
            enforce_monomorphization_budget(&pipeline, &monomorphization_budget, output);
        }

        // Library-artifact C-ABI honesty gate (additive-interop Slice 4):
        // an exported signature whose return/params cross the boundary as
        // neither a transparent-by-value type nor an auto-boxable
        // `Vec`/`String` would emit a dishonest `KaraHandle` while codegen
        // returns/expects a multi-register aggregate — a silent miscompile.
        // Reject before codegen so the produced `.a`/`.so`/`.h` is always
        // ABI-honest. Only fires for a library build (the export IS the C
        // surface); an executable's `pub extern "C" fn` called only from
        // Kāra keeps the internal ABI.
        if crate_type != NativeCrateType::Bin {
            let export_errs = crate::cheader::validate_exports(&pipeline.parsed.program);
            if !export_errs.is_empty() {
                for (fn_name, reason) in &export_errs {
                    eprintln!("error[E_EXPORT_ABI]: exported `{fn_name}`: {reason}");
                }
                process::exit(1);
            }
        }

        // Phase-10: effect findings stay non-fatal for native builds (the
        // long-standing build/check asymmetry, documented at the `karac
        // run`-vs-effects tracker entry), but a target-gate violation
        // (E0411) on a wasm build is different in kind — it proves the
        // program reaches a host resource this target cannot provide, so
        // letting it through just converts a precise diagnostic into an
        // undefined-symbol linker error (or silent misbehavior). Abort
        // with the real message instead.
        if is_wasm {
            if let Some(ref effects) = pipeline.effects {
                let gate_errors: Vec<_> = effects
                    .errors
                    .iter()
                    .filter(|e| e.kind == EffectErrorKind::TargetGateViolation)
                    .collect();
                if !gate_errors.is_empty() {
                    for e in &gate_errors {
                        eprintln!(
                            "error[E0411]: {}:{}:{}: {}",
                            filename, e.span.line, e.span.column, e.message
                        );
                    }
                    process::exit(1);
                }
            }
        }

        // Output executable name — the stem derived before the
        // component-bindings setup above.
        // Scratch object path: `temp_dir()` + PID + stem, mirroring the
        // project-mode build (`cmd_build_project`). Keying on the stem alone
        // (`/tmp/karac_<stem>.o`) let two concurrent `karac build` invocations
        // with the same stem clobber each other's intermediate — a real race
        // for parallel build systems (`make -j`) and the cause of flaky
        // parallel `cargo test` wasm runs. PID disambiguates concurrent
        // processes (each invocation is its own process).
        let obj_path = std::env::temp_dir()
            .join(format!("karac_{}_{exe_name}.o", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let exe_path = if let Some(explicit) = out_path {
            // `-o <path>` / `--out <path>`. Until B-2026-07-31-36 this branch
            // did not exist: the flag parsed, so it drew no "unknown flag"
            // error, but the executable path never consulted it and the
            // artifact still landed at `<stem>` in the CWD. Silently writing
            // somewhere other than where the user pointed is the worst of the
            // three options — worse than rejecting the flag — and it left no
            // way at all to redirect a build, which is why stray `karac build`
            // outputs kept accumulating at the repo root.
            //
            // Honored verbatim, `cc -o` style: no extension is appended, on
            // any target. An explicit path is the user's to name.
            explicit.to_string()
        } else if is_wasm {
            // WASI command module — the artifact is loaded by a wasm
            // host, never exec'd directly, so it always carries the
            // extension. (`dist/wasm/<pkg>.wasm` layout is project
            // mode's concern — the artifact-emission tracker entry.)
            format!("{exe_name}.wasm")
        } else if cfg!(windows) {
            format!("{exe_name}.exe")
        } else {
            exe_name.to_string()
        };

        // phase-8 `panic = "unwind" | "abort"` slice 2: the v1 backend is
        // abort-only. Reject an explicit `panic = "unwind"` before codegen.
        if let Err(e) =
            reject_unsupported_panic_strategy(manifest_panic_strategy_for(filename, output))
        {
            eprintln!("error: {e}");
            process::exit(1);
        }

        if let Err(e) = crate::codegen::compile_to_object_with_hot_swap(
            &pipeline.parsed.program,
            &obj_path,
            pipeline.ownership.as_ref(),
            // WASM concurrency lowering (sequential default / wasm-threads)
            // is its own phase-10 entry — until it lands, suppress the
            // auto-par groups so wasm modules lower sequentially instead of
            // emitting spawn-site calls into a runtime archive that has no
            // scheduler.
            if is_wasm {
                None
            } else {
                pipeline.concurrency.as_ref()
            },
            Some(filename),
            Some(&source),
            enable_hot_swap,
            release,
            true, // A2: coroutines on for `karac build` (bug-C fix reaches real builds)
        ) {
            eprintln!("error: codegen failed: {e}");
            process::exit(1);
        }

        // Library-artifact producer mode (additive-interop Slice 2 + 3;
        // design.md § Exported C ABI). The emitted object carries the
        // program's `pub extern "C" fn` surface with External linkage +
        // bare C symbols; archive/link it into a `.a`/`.so`/`.dylib` and
        // emit the companion C header, instead of linking an executable.
        // Native-only (guaranteed: the wasm × crate-type combination was
        // rejected above). Returns from `cmd_build` — the wasm/exe link
        // tail below is `Bin`-only.
        if crate_type != NativeCrateType::Bin {
            // Export-boundary effect violations are FATAL for a library
            // artifact — the exported C surface IS the deliverable, so
            // unlike an executable (where native effect findings are
            // non-fatal, the long-standing build/check asymmetry) a
            // suspending export must stop the build rather than ship a
            // library that misbehaves on a bare foreign thread. (The
            // C-unwind-export case is already caught earlier at codegen.)
            if let Some(effects) = pipeline.effects.as_ref() {
                let export_errs: Vec<_> = effects
                    .errors
                    .iter()
                    .filter(|e| e.kind == EffectErrorKind::ExternExportSuspendsUnsupported)
                    .collect();
                if !export_errs.is_empty() {
                    for e in &export_errs {
                        eprintln!(
                            "error[E0414]: {}:{}:{}: {}",
                            filename, e.span.line, e.span.column, e.message
                        );
                    }
                    let _ = std::fs::remove_file(&obj_path);
                    process::exit(1);
                }
            }
            let lib_kind = match crate_type {
                NativeCrateType::StaticLib => crate::codegen::NativeLibKind::StaticLib,
                NativeCrateType::CDylib => crate::codegen::NativeLibKind::CDylib,
                NativeCrateType::Bin => unreachable!(),
            };
            // Default artifact path: `lib<stem>.<ext>` in CWD — a name
            // distinct from the `<stem>` executable, so a library build
            // never clobbers a stray binary (the producer-mode gotcha).
            // `-o <path>` overrides verbatim.
            let default_name = format!("lib{exe_name}{}", lib_kind.artifact_extension());
            let art_path = out_path.map(str::to_string).unwrap_or(default_name);
            // Symbols the artifact must publish — needed for the Windows DLL
            // `/EXPORT:` list (a no-op on unix, which exports every
            // default-visibility symbol). AST-derived so it stays in lockstep
            // with the emitted C header.
            let export_syms = crate::cheader::export_symbols(&pipeline.parsed.program);
            if let Err(e) = crate::codegen::link_native_library(
                &obj_path,
                &art_path,
                lib_kind,
                exe_name,
                &export_syms,
            ) {
                eprintln!("error: link failed: {e}");
                let _ = std::fs::remove_file(&obj_path);
                process::exit(1);
            }
            let _ = std::fs::remove_file(&obj_path);
            // Emit the companion C header next to the artifact (Slice 3):
            // `<artifact-dir>/lib<stem>.h`. `--no-header` is a follow-up;
            // at this slice the header always rides along.
            let header_path = std::path::Path::new(&art_path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|dir| dir.join(format!("lib{exe_name}.h")))
                .unwrap_or_else(|| std::path::PathBuf::from(format!("lib{exe_name}.h")));
            let header = crate::cheader::emit_c_header(&pipeline.parsed.program, exe_name);
            match std::fs::write(&header_path, header) {
                Ok(()) => {
                    println!("Built: {art_path}");
                    println!("Built: {}", header_path.display());
                }
                Err(e) => {
                    eprintln!(
                        "warning: library `{art_path}` built, but writing the C header to {} failed: {e}",
                        header_path.display()
                    );
                    println!("Built: {art_path}");
                }
            }
            print_staticlib_rust_host_note(crate_type);
            return;
        }

        // For embedded component bindings, wasm-ld's output is an
        // intermediate — link the C-ABI core module to a scratch path,
        // then lift it into the single component at `exe_path` below. The
        // scratch basename is source-derived (not pid-bearing) so the
        // module name wasm-ld embeds — and the component carries — is
        // reproducible across rebuilds (B-2026-06-22-3); the enclosing
        // dir carries the per-process uniqueness.
        let (link_scratch_dir, link_out) = if wasm_tools.is_some() {
            match crate::componentize::link_core_scratch(exe_name) {
                Ok((dir, core)) => (Some(dir), core.to_string_lossy().into_owned()),
                Err(e) => {
                    eprintln!("error: link failed: {e}");
                    let _ = std::fs::remove_file(&obj_path);
                    process::exit(1);
                }
            }
        } else {
            (None, exe_path.clone())
        };
        let wasm_export_names =
            crate::wasm_exports::link_export_names(&crate::wasm_exports::collect_wasm_exports(
                &pipeline.parsed.program,
                crate::target::active_target(),
            ));
        match crate::codegen::link_executable_exports(&obj_path, &link_out, &wasm_export_names) {
            Err(e) => {
                eprintln!("error: link failed: {e}");
                let _ = std::fs::remove_file(&obj_path);
                process::exit(1);
            }
            Ok(()) => {
                let _ = std::fs::remove_file(&obj_path);
                if let Some(tool) = &wasm_tools {
                    let host_fns = crate::wasm_glue::collect_host_fns(&pipeline.parsed.program);
                    let wasm_exports = crate::wasm_exports::collect_wasm_exports(
                        &pipeline.parsed.program,
                        crate::target::active_target(),
                    );
                    warn_unlowered_exports(
                        &wasm_exports,
                        crate::wasm_exports::ExportSig::component_lowerable,
                    );
                    let result = crate::componentize::componentize(
                        tool,
                        std::path::Path::new(&link_out),
                        &host_fns,
                        &wasm_exports,
                        exe_name,
                        std::path::Path::new(&exe_path),
                    );
                    if let Some(dir) = &link_scratch_dir {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    if let Err(e) = result {
                        eprintln!("error: componentize failed: {e}");
                        process::exit(1);
                    }
                }
                // Companion artifacts keyed on the resolved bindings
                // mode — not the target name: `--target=wasm_browser
                // --bindings=none` suppresses them (raw module) and
                // `--target=wasm_wasi --bindings=browser` opts a wasi
                // module in (browser/none both lower host fns to
                // the same `kara_host` import entries, so each
                // companion is target-agnostic). Browser bindings ship
                // the ES-module glue (host fn import plumbing + WASI
                // preview-1 polyfill; see `wasm_glue`) plus its
                // TypeScript declarations; embedded component bindings
                // ship NO companion — `<stem>.wasm` is the single
                // self-describing component. The `(json key, path)`
                // pairs feed both output modes.
                let mut companions: Vec<(&str, String)> = Vec::new();
                // `--features wasm-threads`: the dual artifact's second
                // pass — same front-end output, auto-par re-enabled,
                // wasip1-threads machine, --shared-memory link against
                // the threaded runtime archive. Runs after the
                // sequential link so a clean build always has the
                // fallback module on disk first.
                let threads_glue_cfg = if wasm_threads {
                    let threads_filename = format!("{exe_name}.threads.wasm");
                    let threads_obj = std::env::temp_dir()
                        .join(format!("karac_{}_{exe_name}.threads.o", std::process::id()));
                    let cfg = emit_wasm_threads_artifact(
                        &pipeline.parsed.program,
                        pipeline.ownership.as_ref(),
                        pipeline.concurrency.as_ref(),
                        Some(filename),
                        Some(&source),
                        release,
                        &threads_obj.to_string_lossy(),
                        std::path::Path::new(&threads_filename),
                        &threads_filename,
                        manifest_wasm_knobs_for(filename, output),
                    );
                    companions.push(("threads_wasm", threads_filename));
                    Some(cfg)
                } else {
                    None
                };
                match effective_bindings {
                    Some(BindingsMode::Browser) => {
                        let host_fns = crate::wasm_glue::collect_host_fns(&pipeline.parsed.program);
                        let wasm_exports = crate::wasm_exports::collect_wasm_exports(
                            &pipeline.parsed.program,
                            crate::target::active_target(),
                        );
                        warn_unlowered_exports(
                            &wasm_exports,
                            crate::wasm_exports::ExportSig::component_lowerable,
                        );
                        let glue = crate::wasm_glue::render_glue(
                            &host_fns,
                            &wasm_exports,
                            &exe_path,
                            threads_glue_cfg.as_ref(),
                        );
                        let js_path = format!("{exe_name}.js");
                        if let Err(e) = std::fs::write(&js_path, glue) {
                            eprintln!("error: failed to write JS glue {js_path}: {e}");
                            process::exit(1);
                        }
                        companions.push(("glue", js_path));
                        let dts = crate::wasm_glue::render_dts(
                            &host_fns,
                            &wasm_exports,
                            &exe_path,
                            threads_glue_cfg.is_some(),
                        );
                        let dts_path = format!("{exe_name}.d.ts");
                        if let Err(e) = std::fs::write(&dts_path, dts) {
                            eprintln!("error: failed to write TS declarations {dts_path}: {e}");
                            process::exit(1);
                        }
                        companions.push(("dts", dts_path));
                    }
                    Some(BindingsMode::Component) | Some(BindingsMode::None) | None => {}
                }
                // Strip DWARF debug info from emitted .wasm artifacts. wasm-ld
                // keeps the `.debug_*` custom sections (the native link path
                // strips by default; the wasm path does not), and they are
                // ~90%+ of an unstripped module — a 482 KiB browser hello is
                // 93% DWARF, collapsing to ~30 KiB. Strip by default for every
                // wasm artifact (the main module/component plus any
                // `.threads.wasm` sibling); `KARAC_WASM_KEEP_DEBUG=1` opts out
                // for source-level wasm debugging. Best-effort: Component
                // bindings already resolved+required the tool above; for
                // browser/raw builds resolve it lazily here, and a missing or
                // failed strip is a warning, never a build failure.
                if is_wasm && std::env::var_os("KARAC_WASM_KEEP_DEBUG").is_none() {
                    let strip_tool = wasm_tools.clone().or_else(|| {
                        let pin = manifest_release_field_for(filename, output, |m| {
                            m.toolchain_wasm_tools.clone()
                        });
                        crate::componentize::resolve_wasm_tools(pin.as_deref()).ok()
                    });
                    match strip_tool {
                        Some(tool) => {
                            let mut artifacts = vec![exe_path.clone()];
                            artifacts.extend(
                                companions
                                    .iter()
                                    .filter(|(k, _)| *k == "threads_wasm")
                                    .map(|(_, p)| p.clone()),
                            );
                            for artifact in &artifacts {
                                if let Err(e) = crate::componentize::strip_debug(
                                    &tool,
                                    std::path::Path::new(artifact),
                                ) {
                                    eprintln!(
                                        "warning: wasm debug-strip skipped for {artifact}: {e}"
                                    );
                                }
                            }
                        }
                        None => eprintln!(
                            "note: wasm-tools not found — emitted .wasm retains debug info \
                             (install wasm-tools for ~10x smaller modules, or set \
                             KARAC_WASM_KEEP_DEBUG=1 to silence this note)"
                        ),
                    }
                }
                match output {
                    OutputMode::Text => {
                        let mut line = format!("Built: {exe_path}");
                        for (_, path) in &companions {
                            line.push_str(&format!(" + {path}"));
                        }
                        println!("{line}");
                    }
                    OutputMode::Json => {
                        let mut fields = format!("{{\"status\":\"ok\",\"output\":\"{exe_path}\"");
                        for (key, path) in &companions {
                            fields.push_str(&format!(",\"{key}\":\"{path}\""));
                        }
                        // B-2026-08-18-1 — warnings ride the success object,
                        // and ONLY when there are some. A consumer that reads
                        // `status`/`output` is untouched by an absent key,
                        // which is what keeps the kata corpus and the Mend
                        // baselines reading this exactly as they did.
                        if !build_warnings_json.is_empty() {
                            fields.push_str(&format!(
                                ",\"diagnostics\":[{}]",
                                build_warnings_json.join(",")
                            ));
                        }
                        fields.push('}');
                        println!("{fields}");
                    }
                    OutputMode::Jsonl => unreachable!(),
                }
            }
        }
    }
    #[cfg(not(feature = "llvm"))]
    {
        let _ = build_target;
        let _ = enable_hot_swap;
        // `--bindings` only shapes WASM artifact emission, which rides
        // the llvm build path — accepted-but-inert here, consistent
        // with --offline / --target above.
        let _ = bindings;
        // `--target-cpu` / `--target-features` only parameterize the
        // LLVM target machine — accepted-but-inert on the non-llvm
        // check fallback.
        let _ = target_cpu;
        let _ = target_features;
        // `--release` only affects codegen (contract stripping), which the
        // non-llvm fallback doesn't reach — accepted-but-inert, consistent
        // with --offline / --target / --enable-hot-swap above.
        let _ = release;
        // The budget check rides the llvm build path (after typecheck,
        // before codegen); the non-llvm fallback type-checks only, so the
        // flag is accepted-but-inert here, consistent with --offline /
        // --target.
        let _ = monomorphization_budget;
        // `--features wasm-threads` shapes WASM codegen+link, which the
        // non-llvm fallback doesn't reach — accepted-but-inert, the
        // `--bindings` posture.
        let _ = wasm_threads;
        // `--crate-type staticlib/cdylib` + `-o` drive the producer-mode
        // library link path, which rides the llvm build — accepted-but-
        // inert on the non-llvm check fallback.
        let _ = crate_type;
        let _ = out_path;
        eprintln!("note: karac build requires the llvm feature; falling back to type check");
        cmd_check(
            filename,
            output,
            None,
            None,
            concurrency_report,
            simd_report,
            lint_overrides,
        );
    }
}

/// Enforce a `--monomorphization-budget` ceiling after typecheck. Human-
/// readable `warning[monomorphization-budget]` / `error[…]` diagnostics
/// go to stderr (keeping stdout reserved for the build result). Any
/// error-level violation fails the build with status 1 before codegen
/// runs — in JSON mode it also emits a diagnostics envelope on stdout,
/// mirroring the `has_fatal_errors` JSON path. The caller gates on
/// `is_enabled`, so a disabled budget never reaches here.
#[cfg(feature = "llvm")]
pub(super) fn enforce_monomorphization_budget(
    pipeline: &Pipeline,
    budget: &crate::monomorphization::MonomorphizationBudget,
    output: OutputMode,
) {
    use crate::monomorphization::{BudgetLevel, BudgetViolation};

    let Some(tc) = pipeline.typed.as_ref() else {
        return;
    };
    let table =
        crate::monomorphization::analyze(&pipeline.parsed.program, tc, pipeline.effects.as_ref());
    let violations = table.budget_violations(budget);
    if violations.is_empty() {
        return;
    }

    let render = |v: &BudgetViolation| {
        let kind = match v.level {
            BudgetLevel::Error => "error",
            BudgetLevel::Warning => "warning",
        };
        format!(
            "{kind}[monomorphization-budget]: {}:{}:{}: generic `{}` has {} instantiations (limit {})",
            pipeline.filename, v.site.line, v.site.column, v.generic, v.count, v.threshold
        )
    };

    // Human-readable diagnostics (warnings and errors alike) always go to
    // stderr so stdout stays reserved for the single build-result line.
    for v in &violations {
        eprintln!("{}", render(v));
    }

    let errors: Vec<&BudgetViolation> = violations
        .iter()
        .filter(|v| v.level == BudgetLevel::Error)
        .collect();
    if errors.is_empty() {
        // Warn-only: the build continues to codegen.
        return;
    }

    match output {
        OutputMode::Text => process::exit(1),
        OutputMode::Json => {
            let diags: Vec<String> = errors
                .iter()
                .map(|v| {
                    format!(
                        "{{\"severity\":\"error\",\"phase\":\"monomorphization-budget\",\"generic\":{},\"count\":{},\"limit\":{},\"site\":{}}}",
                        json_string(&v.generic),
                        v.count,
                        v.threshold,
                        json_string(&format!(
                            "{}:{}:{}",
                            pipeline.filename, v.site.line, v.site.column
                        )),
                    )
                })
                .collect();
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{}]}}",
                diags.join(",")
            );
            process::exit(1);
        }
        OutputMode::Jsonl => unreachable!(),
    }
}

/// Project-mode build entry point.
///
/// Discovers the project root via `kara.toml` walk-up, loads the manifest,
/// walks `src/` to map each `.kara` file to a module path (CR-24 slice 3),
/// parses every file into its own `Program`, assembles the module graph
/// Render documentation for the current project. v1 MVP — walks the
/// project tree, parses every module, and emits one HTML page per
/// documented item under `dist/doc/`. Items without `///` doc comments
/// are skipped silently. Resolver / typechecker passes are intentionally
/// not run: doc rendering only needs the AST surface, and producing
/// docs against a project that doesn't fully type-check is useful for
/// a programmer trying to understand half-finished code.
pub(super) fn cmd_doc() {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, _mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let walked = match walker::walk_project(&root, WalkerOpts::default()) {
        Ok(w) => w,
        Err(e) => {
            emit_walker_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let built = match module::build_program_tree(&walked) {
        Ok(ok) => ok,
        Err(e) => {
            emit_build_tree_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let BuildTreeOk { tree, parse_errors } = built;
    if !parse_errors.is_empty() {
        // Surface parse errors but keep going — render docs for what
        // parsed cleanly. The user can iterate.
        print_parse_errors_text(&parse_errors);
    }

    // Run effectcheck once on a merged Program containing every
    // non-synthetic module's items so cross-module callee resolution
    // works at the bare-name level the effectchecker indexes by. See
    // `build_doc_effects_table` for the trade-offs.
    let effects = build_doc_effects_table(&tree);

    let output_dir = root.join("dist").join("doc");
    match crate::doc::build_docs(&tree, &output_dir, Some(&effects)) {
        Ok(result) => {
            println!(
                "rendered {} doc page(s) under {}",
                result.written.len().saturating_sub(1), // minus the index
                output_dir.display()
            );
        }
        Err(e) => {
            eprintln!("error[doc]: {e}");
            process::exit(1);
        }
    }
}

/// Build the `(module_path, fn_name) → EffectDisplay` table consumed
/// by the doc renderer.
///
/// Strategy: merge every non-synthetic module's items into a single
/// `Program` and run `effectcheck` once. The effectchecker indexes
/// functions by bare name, and cross-module call sites also resolve
/// to bare names (Kāra's `import` brings a callee into scope under
/// its bare name). A per-module check would treat every cross-module
/// call as effect-empty — `pub fn`s whose inferred set depends on a
/// callee in another module would surface incomplete `with` clauses
/// in the rendered docs.
///
/// Trade-off: when two modules define functions with the same bare
/// name, the merge keeps only one and the doc display is approximate.
/// This is doc-only; the main pipeline (`build`, `check`, `run`)
/// still runs effectcheck per-module via the regular phase wiring.
/// Effectcheck errors raised by the merged pass (e.g. duplicate
/// resource declarations across modules, missing effect declarations)
/// are deliberately ignored here — the doc renderer is best-effort.
pub(super) fn build_doc_effects_table(tree: &ProgramTree) -> crate::doc::EffectsByItem {
    use crate::ast::Item;
    use crate::doc::{EffectDisplay, EffectsByItem};
    use crate::effectchecker::{DeclaredEffects, EffectSet};

    let mut merged_items = Vec::new();
    for module in &tree.modules {
        if module.is_synthetic {
            continue;
        }
        merged_items.extend(module.items.iter().cloned());
    }
    let merged_program = Program {
        items: merged_items,
        ..Program::default()
    };
    let effects = crate::effectcheck(&merged_program);

    let mut out: EffectsByItem = std::collections::HashMap::new();
    for module in &tree.modules {
        if module.is_synthetic {
            continue;
        }
        for item in &module.items {
            let Item::Function(f) = item else { continue };
            if !f.is_pub {
                continue;
            }

            // Prefer the declared annotation (the user's contract);
            // fall back to the inferred set if no explicit annotation.
            let display = match effects.declared_effects.get(&f.name) {
                Some(DeclaredEffects::Explicit(set)) => effect_set_to_display(set, false),
                Some(DeclaredEffects::Polymorphic) => EffectDisplay {
                    effects: Vec::new(),
                    polymorphic: true,
                },
                Some(DeclaredEffects::PolymorphicWithFixed(set)) => {
                    effect_set_to_display(set, true)
                }
                Some(DeclaredEffects::None) | None => effects
                    .inferred_effects
                    .get(&f.name)
                    .map(|set: &EffectSet| effect_set_to_display(set, false))
                    .unwrap_or_default(),
            };

            if !display.effects.is_empty() || display.polymorphic {
                out.insert((module.path.clone(), f.name.clone()), display);
            }
        }
    }

    out
}

pub(super) fn effect_set_to_display(
    set: &crate::effectchecker::EffectSet,
    polymorphic: bool,
) -> crate::doc::EffectDisplay {
    let mut effects: Vec<(crate::ast::EffectVerbKind, String)> = set
        .effects
        .iter()
        .map(|t| (t.effect.verb.clone(), t.effect.resource.clone()))
        .collect();
    // Stable order across runs: by verb name, then resource.
    effects.sort_by(|a, b| {
        let an = effect_verb_str(&a.0);
        let bn = effect_verb_str(&b.0);
        an.cmp(bn).then_with(|| a.1.cmp(&b.1))
    });
    crate::doc::EffectDisplay {
        effects,
        polymorphic,
    }
}

/// (slice 4), runs Tarjan's SCC to reject circular module dependencies
/// (`E0223`), and runs cross-module name resolution per module
/// (slice 5, `E0112` / `E0113`). Visibility enforcement and typechecking
/// across modules arrive in slice 6+.
// Same flag-shaped-argument posture as `cmd_build` above — a struct
// here would just move the flag list rather than tighten it.
#[allow(clippy::too_many_arguments)]
pub(super) fn cmd_build_project(
    output: OutputMode,
    offline: bool,
    enable_hot_swap: bool,
    no_proxy: bool,
    target: Option<&str>,
    bindings: Option<BindingsMode>,
    target_cpu: Option<&str>,
    target_features: Option<&str>,
    wasm_threads: bool,
    release: bool,
    crate_type: NativeCrateType,
    out_path: Option<&str>,
    // B-2026-08-18-19 — the CLI's `-A` / `-W` / `-D` levels. This function did
    // not take them at all: it built its per-module overrides from
    // `CliLintOverrides::default()` plus the manifest, so a project build
    // ignored every lint flag the invocation carried. Harmless-looking while
    // the only project-mode consumer was error-severity, and immediately wrong
    // once project builds started rendering WARNINGS — `karac build -A
    // deprecated` printed the warning anyway.
    lint_overrides: crate::lints::CliLintOverrides,
) {
    // Phase-10: v1 target names are classified the same way as in
    // single-file mode. A wasm name selects the project-mode WASM build:
    // super-program codegen → wasm-ld → the `dist/wasm/<pkg>.wasm`
    // artifact layout (+ `<pkg>.js` / `<pkg>.d.ts` under browser
    // bindings — the "WASM browser artifact emission" entry). Triples
    // pass through to the manifest `[target.<triple>.*]` overlay merge
    // below unchanged.
    let build_target = resolve_build_target(target);
    let is_wasm = build_target == "wasm_wasi" || build_target == "wasm_browser";
    let effective_bindings = resolve_effective_bindings(build_target, bindings);
    // Hot-swap requires dynamic symbol resolution at runtime; a wasm
    // module has none (no dlopen in a browser/WASI host). This is the
    // wasm half of the phase-7 hot-swap target gating, actionable now
    // that `--target=wasm_*` reaches project mode.
    if enable_hot_swap && is_wasm {
        eprintln!(
            "error: --enable-hot-swap is incompatible with --target={build_target} \
             (no dynamic-symbol-resolution machinery on wasm hosts)"
        );
        process::exit(1);
    }
    // `--features wasm-threads` scope gate — single-file contract
    // (see `validate_wasm_threads_scope`): wasm_browser-only, no
    // component bindings. Runs pre-manifest so the failure mode is
    // identical from any directory — and llvm-independent, so a
    // non-llvm build rejects the flag here rather than tripping the
    // manifest-not-found check below.
    validate_wasm_threads_scope(wasm_threads, build_target, effective_bindings);
    // Phase-10 WASM entry-point discovery: browser + component bindings
    // marshal rich exports (canonical-ABI trampolines); `--bindings none`
    // keeps raw core exports. Signal codegen before it runs.
    crate::target::set_wasm_export_marshalling(matches!(
        effective_bindings,
        Some(BindingsMode::Browser) | Some(BindingsMode::Component)
    ));
    // `--target-cpu=help` / `--target-features=help` exit before
    // manifest discovery so the listing works from any directory — it
    // needs only the active target, not a project. Name validation for
    // a real value waits until the manifest is loaded (the `[release]`
    // tier of each precedence chain lives there); see below.
    #[cfg(feature = "llvm")]
    if target_cpu == Some("help") || target_features == Some("help") {
        crate::codegen::print_target_cpu_listing();
        process::exit(0);
    }
    // --offline implies --no-proxy at the contract level (vendor-only walk
    // can't talk to the proxy). Suppress the redundant no-proxy note when
    // both are set so the offline operator sees one clean status line.
    if !offline {
        emit_no_proxy_note(no_proxy);
    }
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, raw_manifest) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, output);
            process::exit(1);
        }
    };

    // Toolchain pin (tracker line 892). When `karac-toolchain.toml`
    // exists somewhere in the project's ancestor chain, the active
    // compiler version must satisfy the declared constraint. Halts
    // the build with a focused diagnostic on mismatch — no auto-
    // switch (that's the karaup follow-up).
    if !enforce_toolchain_pin(&root, output) {
        process::exit(1);
    }

    // Resolve the active target triple for `[target.<triple>.*]` overlay
    // selection (tracker line 882). Precedence: `--target=<triple>` >
    // `[build].target` > host triple. Recorded as a single owned value
    // so the overlay merge consumes a stable reference. A v1 target
    // *name* is not a triple: a wasm name pins the overlay triple to
    // the real compilation triple (`wasm32-wasip1` — both wasm names
    // build the same module flavor), and an explicit `native` pins the
    // host triple (an explicit flag outranks `[build].target`, the
    // chain's documented precedence).
    let active_target: String = match target {
        Some(t) if !crate::target::is_v1_target_name(t) => t.to_string(),
        Some(_) if is_wasm => "wasm32-wasip1".to_string(),
        Some(_) => crate::build_cache::host_target_triple(),
        None => raw_manifest
            .build_default_target
            .clone()
            .unwrap_or_else(crate::build_cache::host_target_triple),
    };

    // Merge `[target.<triple>].dependencies` / `[target.<triple>].profile`
    // overlays onto the manifest before any downstream consumer reads it
    // (dep resolution, profile gating, codegen). Always applied with the
    // resolved active triple so the build sees one consistent view.
    let mf = manifest::merge_target_overlay(&raw_manifest, Some(active_target.as_str()));

    // CPU baseline override — same precedence chain as single-file mode
    // (flag > `KARAC_TARGET_CPU` > `[release] target-cpu`), with the
    // manifest tier read from the project's own manifest (already
    // loaded) instead of a file-relative walk-up. Installed before
    // codegen runs; `help` was handled above, pre-discovery.
    // `[release] cpu-baseline` (arch-portable, lowest tier) maps to a concrete
    // native override in DIFFERENT channels per arch: a target-CPU on x86-64
    // (`x86-64-vN`), an architecture-version target-FEATURE on aarch64
    // (`+v8.Na`). The absent-key default is `v3` (design.md § Multiversioning).
    #[cfg(feature = "llvm")]
    let (baseline_cpu, baseline_features) =
        resolve_native_cpu_baseline(mf.release_cpu_baseline.as_deref());
    #[cfg(feature = "llvm")]
    apply_target_cpu_override(
        target_cpu
            .map(str::to_string)
            .or_else(read_target_cpu_env)
            .or_else(|| mf.release_target_cpu.clone())
            .or(baseline_cpu),
    );
    // Feature-string override — the independent sibling chain.
    #[cfg(feature = "llvm")]
    apply_target_features_override(
        target_features
            .map(str::to_string)
            .or_else(read_target_features_env)
            .or_else(|| mf.release_target_features.clone())
            .or(baseline_features),
    );
    #[cfg(not(feature = "llvm"))]
    let _ = (target_cpu, target_features, out_path);

    // External native-library linking (`[link]` table) — native targets
    // only (wasm-ld ignores it). Read from the project's own manifest
    // (already loaded), no walk-up. Installed before codegen runs.
    #[cfg(feature = "llvm")]
    if !is_wasm {
        apply_native_link_config(mf.link_libs.clone(), mf.link_search_paths.clone());
    }

    // Embedded-WIT component bindings — the single-file `cmd_build`
    // contract: resolve the external wasm-tools up front (failing fast
    // on missing/mis-pinned, pin from the project's own `[toolchain]`
    // table — already loaded, no walk-up needed) and install the
    // package name that flips codegen's host-fn import attachment to
    // canonical-ABI `kara:<pkg>/host` naming. Runtime-
    // gated on the llvm feature: the non-llvm fallback builds nothing,
    // so a missing tool must not fail what is effectively a check run.
    let wasm_tools = if cfg!(feature = "llvm") {
        match effective_bindings {
            Some(BindingsMode::Component) => {
                match crate::componentize::resolve_wasm_tools(mf.toolchain_wasm_tools.as_deref()) {
                    Ok(tool) => {
                        crate::target::set_wasm_component_host_package(&mf.name);
                        Some(tool)
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        process::exit(1);
                    }
                }
            }
            _ => None,
        }
    } else {
        None
    };

    // Phase-7 line 5 sub-item 3 — target gating. Hot-swap requires dynamic
    // symbol resolution at runtime, which embedded and kernel profiles
    // do not provide. Reject the combination before any work.
    // The wasm-target half of the entry's gating defers until a wasm
    // CompileProfile (or `--target=`) lands; no enum variant to gate
    // against yet. Reads `mf.profile` post-overlay so a target-specific
    // override is honored here.
    if enable_hot_swap
        && matches!(
            mf.profile,
            crate::manifest::CompileProfile::Embedded | crate::manifest::CompileProfile::Kernel
        )
    {
        eprintln!(
            "error: --enable-hot-swap is incompatible with [package].profile = \"{}\" (no dynamic-symbol-resolution machinery on this profile)",
            mf.profile.as_str()
        );
        process::exit(1);
    }

    // Offline-mode pre-check: the vendor root must exist before the
    // resolver consults it. A missing `./vendor/` is a clear operator
    // mistake — the right action is "run `karac vendor`", not "fix
    // every transitive dep". Skipped when the manifest declares no deps
    // and no MSRV constraint — solo projects pay nothing for `--offline`.
    let has_deps =
        !mf.dependencies.is_empty() || !mf.dev_dependencies.is_empty() || mf.kara_version.is_some();
    let vendor_root_buf = root.join("vendor");
    if offline && has_deps && !vendor_root_buf.is_dir() {
        emit_offline_no_vendor_dir(&vendor_root_buf, output);
        process::exit(1);
    }
    let offline_root: Option<&std::path::Path> = if offline {
        Some(vendor_root_buf.as_path())
    } else {
        None
    };

    // Slice 7 of the PubGrub-resolver entry: validate the dep graph
    // before the walker even runs. Errors halt the build; unsupported-
    // source warnings (registry/git, until fetch ships at line 819)
    // surface as notices and the build continues. Skipped entirely when
    // the manifest declares no deps and no MSRV constraint — the common
    // single-package, no-dep case pays zero overhead.
    // Build mode: dev-dependencies are excluded from resolution
    // (tracker line 884). The test runner re-invokes resolution with
    // `include_dev_deps=true` so `[dev-dependencies]` surface only
    // when actually compiling tests.
    let dep_resolution: Option<crate::dep_resolver::Resolution> = if has_deps {
        match run_dep_resolution(
            &root,
            mf.clone(),
            output,
            offline_root,
            false,
            no_proxy,
            true,
        ) {
            Ok(r) => r,
            Err(()) => process::exit(1),
        }
    } else {
        None
    };

    // Project-mode platform-suffix selection must follow the *build* target,
    // not the host. A `--target=wasm_*` build has to select `_wasm` platform
    // modules (and drop `_macos`/`_linux`/`_windows`), exactly as a single-file
    // cross-target build does — otherwise a wasm build wrongly compiles the
    // host's native platform modules (and omits the wasm ones), so an example
    // that swaps its host/IO layer per target via platform suffixes builds the
    // wrong half. Native builds keep the host platform; cross-triple native
    // selection is a separate concern that `host()` preserves unchanged.
    let walk_opts = WalkerOpts {
        target: if is_wasm {
            walker::Platform::Wasm
        } else {
            walker::Platform::host()
        },
        ..WalkerOpts::default()
    };
    let walked = match walker::walk_project(&root, walk_opts) {
        Ok(w) => w,
        Err(e) => {
            emit_walker_error(&e, output);
            process::exit(1);
        }
    };

    // Cross-package module loading (phase-5 line 898): walk each resolved
    // path-dep's source tree so its modules join the program tree under
    // package-prefixed paths, making `import <pkg>.…` resolve.
    let dep_walks = match dep_package_walks(dep_resolution.as_ref(), walk_opts.target, output) {
        Ok(v) => v,
        Err(()) => process::exit(1),
    };

    let built = match module::build_program_tree_with_deps(
        &walked,
        &dep_walks,
        module::BuildTreeOpts::default(),
    ) {
        Ok(ok) => ok,
        Err(e) => {
            emit_build_tree_error(&e, output);
            process::exit(1);
        }
    };

    let BuildTreeOk { tree, parse_errors } = built;

    let cycles = module::detect_cycles(&tree);

    // Slice 5: run cross-module name resolution per module. Only attempt
    // resolution when the graph is acyclic and every file parsed cleanly —
    // otherwise we would cascade dozens of spurious E0112/E0113s atop the
    // real failure.
    let resolve_errors: Vec<ModuleResolveErrors> = if parse_errors.is_empty() && cycles.is_empty() {
        resolve_modules(&tree)
    } else {
        Vec::new()
    };

    // Slice 6 (follow-up): run the typechecker per module with the project
    // tree attached so cross-module `E0221` and the CR-18 field-access rule
    // can fire. Skipped when earlier phases reported errors, since a half-
    // resolved program produces unhelpful type cascades.
    // Phase-8 line 49 prereq 4 — lift `[lints].allow_unstable_api`
    // from the manifest into a per-module `CliLintOverrides` so the
    // project-build typecheck honors the global opt-in. Today this
    // is the only manifest-driven lint override; future fields land
    // beside it.
    //
    // Seeded from the CLI overrides, not from `default()`. Starting empty threw
    // away every `-A` / `-W` / `-D` the invocation carried, so a project build
    // ignored them outright — invisible while the only project-mode lint
    // consumer was error-severity, and immediately visible once B-2026-08-18-19
    // made project builds RENDER warnings: `karac build -A deprecated` printed
    // the warning anyway, which is worse than not printing it. `cmd_run` and
    // `cmd_check` already compose them in this order, and
    // `apply_manifest_lints` is written for it — it uses `or_insert`, so an
    // explicit CLI level for a lint wins over the manifest's opt-in.
    let mut module_lint_overrides = lint_overrides;
    module_lint_overrides.apply_manifest_lints(&mf.lints);
    let module_diags = if parse_errors.is_empty() && cycles.is_empty() && resolve_errors.is_empty()
    {
        typecheck_modules(&tree, &module_lint_overrides)
    } else {
        ModuleTypeDiagnostics {
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    };
    let type_errors = module_diags.errors;
    let type_warnings = module_diags.warnings;

    // Theme 4 (2026-05-10) — multi-file project-mode codegen. Per-module
    // resolve + typecheck above produce per-file diagnostics; once those
    // pass, the codegen path concatenates all module items (in topological
    // order, dropping `import` declarations + the synthetic prelude) into a
    // single super-program and drives it through the existing single-file
    // pipeline (`lower` → `effectcheck` → `ownershipcheck` →
    // `concurrencycheck` → codegen → link). Per-module wiring of the post-
    // typecheck phases would lose cross-module callee-effect visibility
    // (concurrency analysis depends on knowing imported functions' effects);
    // the super-program approach gives correct cross-module analysis at the
    // cost of less granular file-context in late-phase diagnostics. Symbol
    // mangling deferred to v2 — cross-module function-name collisions
    // surface as duplicate-symbol errors at the LLVM linker (clear failure,
    // ungainly diagnostic; structured detection is a follow-up).
    // Resolve the effective library-artifact kind (additive-interop Slice 2,
    // project-mode `[lib]`): a CLI `--crate-type staticlib/cdylib` wins; else
    // the manifest `[lib] crate-type`; else an executable. A bare/omitted
    // `bin` CLI flag reads as "unset" and falls to the manifest — a project
    // with a `[lib]` table builds a library by default.
    let effective_crate_type = if crate_type != NativeCrateType::Bin {
        crate_type
    } else {
        match mf.lib_crate_type.as_deref() {
            Some("staticlib") => NativeCrateType::StaticLib,
            Some("cdylib") => NativeCrateType::CDylib,
            _ => NativeCrateType::Bin,
        }
    };
    // Library-artifact mode is native-only (single-file posture).
    if is_wasm && effective_crate_type != NativeCrateType::Bin {
        eprintln!(
            "error: --crate-type staticlib/cdylib (or a `[lib]` table) is a native-only producer \
             mode; for a wasm library surface use `--target={build_target}` with `--bindings`."
        );
        process::exit(1);
    }
    let mut codegen_status: BuildCodegenStatus = BuildCodegenStatus::Skipped;
    if !cfg!(feature = "llvm") {
        // Mirror the single-file `cmd_build` no-llvm fallback (line ~2393).
        codegen_status = BuildCodegenStatus::NoLlvmFeature;
    } else if parse_errors.is_empty()
        && cycles.is_empty()
        && resolve_errors.is_empty()
        && type_errors.is_empty()
    {
        codegen_status = run_multi_file_codegen(
            &tree,
            &mf,
            &root,
            enable_hot_swap,
            release,
            is_wasm,
            effective_bindings,
            wasm_tools.as_ref(),
            wasm_threads,
            effective_crate_type,
            out_path,
        );
    }

    let failed = !parse_errors.is_empty()
        || !cycles.is_empty()
        || !resolve_errors.is_empty()
        || !type_errors.is_empty()
        || matches!(codegen_status, BuildCodegenStatus::Failed { .. });

    match output {
        OutputMode::Text => {
            for w in &mf.warnings {
                eprintln!("warning[manifest]: {}", w.message);
            }
            // B-2026-08-18-19 — per-module typecheck warnings. The single-file
            // path gained this in B-2026-08-18-1; here the warnings had been
            // discarded one layer earlier, in `typecheck_modules`, so a project
            // build printed its banner and `Built: …` and nothing else.
            //
            // Rendered alongside the manifest warnings rather than through the
            // shared `render_text_warning_diagnostics`: that helper takes a
            // single-file `Pipeline`, and this path has no such thing at this
            // point — it holds per-module results, each with its OWN file, which
            // is exactly the context a project build has to carry and the
            // single-file one does not.
            print_type_warnings_text(&type_warnings);
            print_parse_errors_text(&parse_errors);
            print_cycles_text(&cycles, &tree);
            print_resolve_errors_text(&resolve_errors);
            print_type_errors_text(&type_errors);
            println!("project: {}", mf.name);
            println!("edition: {}", mf.edition);
            println!("root:    {}", root.display());
            println!("target:  {}", walk_opts.target.as_suffix());
            println!("entry:   {}", entry_label(walked.entry));
            println!("modules: {}", walked.modules.len());
            for m in &walked.modules {
                let path = if m.path.is_empty() {
                    "<crate root>".to_string()
                } else {
                    m.path.join(".")
                };
                let plat = match m.platform {
                    Some(p) => format!(" [{}]", p.as_suffix()),
                    None => String::new(),
                };
                println!("  {path}{plat}  {}", m.file.display());
            }
            if !dep_walks.is_empty() {
                let dep_module_count: usize =
                    dep_walks.iter().map(|d| d.walked.modules.len()).sum();
                println!(
                    "deps:    {} package(s), {} module(s)",
                    dep_walks.len(),
                    dep_module_count
                );
                for d in &dep_walks {
                    println!("  {}  {}", d.name, d.walked.src_dir.display());
                }
            }
            if failed {
                let total = parse_errors.iter().map(|pe| pe.errors.len()).sum::<usize>()
                    + cycles.len()
                    + resolve_errors
                        .iter()
                        .map(|re| re.errors.len())
                        .sum::<usize>()
                    + type_errors.iter().map(|te| te.errors.len()).sum::<usize>()
                    + codegen_status.error_count();
                if let BuildCodegenStatus::Failed { phase, message } = &codegen_status {
                    eprintln!("error[{phase}]: {message}");
                }
                eprintln!("\n{total} error(s) found.");
                process::exit(1);
            }
            match &codegen_status {
                BuildCodegenStatus::Built {
                    exe_path,
                    glue_path,
                    dts_path,
                    threads_wasm_path,
                } => {
                    let mut line = format!("Built: {}", exe_path.display());
                    for extra in [threads_wasm_path, glue_path, dts_path]
                        .into_iter()
                        .flatten()
                    {
                        line.push_str(&format!(" + {}", extra.display()));
                    }
                    println!("{line}");
                }
                BuildCodegenStatus::NoLlvmFeature => {
                    eprintln!(
                        "note: karac build requires the llvm feature; project type-checked but no executable was produced."
                    );
                }
                BuildCodegenStatus::Skipped | BuildCodegenStatus::Failed { .. } => {}
            }
        }
        OutputMode::Json => {
            let warnings: Vec<String> = mf
                .warnings
                .iter()
                .map(|w| {
                    format!(
                        "{{\"severity\":\"warning\",\"phase\":\"manifest\",\"message\":{}}}",
                        json_string(&w.message),
                    )
                })
                .collect();
            let mut diags = warnings;
            // B-2026-08-18-19 — per-module typecheck warnings join the array
            // the manifest warnings already ride. The key exists on this path
            // (unlike the single-file one, where B-2026-08-18-1 had to add it),
            // so a consumer's shape does not change at all.
            diags.extend(type_warnings_json(&type_warnings));
            diags.extend(parse_errors_json(&parse_errors));
            diags.extend(cycles_json(&cycles, &tree));
            diags.extend(resolve_errors_json(&resolve_errors));
            diags.extend(type_errors_json(&type_errors));
            if let BuildCodegenStatus::Failed { phase, message } = &codegen_status {
                diags.push(format!(
                    "{{\"severity\":\"error\",\"phase\":{},\"message\":{}}}",
                    json_string(phase),
                    json_string(message),
                ));
            }
            let modules = render_walked_modules_json(&walked);
            let status = if failed { "error" } else { "ok" };
            let output_field = match &codegen_status {
                BuildCodegenStatus::Built {
                    exe_path,
                    glue_path,
                    dts_path,
                    threads_wasm_path,
                } => {
                    let mut field = format!(
                        ",\"output\":{}",
                        json_string(&exe_path.display().to_string())
                    );
                    if let Some(tw) = threads_wasm_path {
                        field.push_str(&format!(
                            ",\"threads_wasm\":{}",
                            json_string(&tw.display().to_string())
                        ));
                    }
                    if let Some(js) = glue_path {
                        field.push_str(&format!(
                            ",\"glue\":{}",
                            json_string(&js.display().to_string())
                        ));
                    }
                    if let Some(dts) = dts_path {
                        field.push_str(&format!(
                            ",\"dts\":{}",
                            json_string(&dts.display().to_string())
                        ));
                    }
                    field
                }
                _ => String::new(),
            };
            println!(
                "{{\"status\":{},\"project\":{},\"edition\":{},\"root\":{},\"target\":{},\"entry\":{},\"modules\":[{}],\"diagnostics\":[{}]{}}}",
                json_string(status),
                json_string(&mf.name),
                json_string(&mf.edition),
                json_string(&root.display().to_string()),
                json_string(walk_opts.target.as_suffix()),
                json_string(entry_label(walked.entry)),
                modules,
                diags.join(","),
                output_field,
            );
            if failed {
                process::exit(1);
            }
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "manifest_loaded",
                &format!(
                    "\"project\":{},\"edition\":{},\"root\":{}",
                    json_string(&mf.name),
                    json_string(&mf.edition),
                    json_string(&root.display().to_string()),
                ),
            );
            for w in &mf.warnings {
                emit_jsonl_event(
                    "manifest_warning",
                    &format!("\"message\":{}", json_string(&w.message)),
                );
            }
            let modules = render_walked_modules_json(&walked);
            emit_jsonl_event(
                "modules_discovered",
                &format!(
                    "\"target\":{},\"entry\":{},\"modules\":[{}]",
                    json_string(walk_opts.target.as_suffix()),
                    json_string(entry_label(walked.entry)),
                    modules,
                ),
            );
            for entry in parse_errors_jsonl(&parse_errors) {
                println!("{entry}");
            }
            for entry in cycles_jsonl(&cycles, &tree) {
                println!("{entry}");
            }
            for entry in resolve_errors_jsonl(&resolve_errors) {
                println!("{entry}");
            }
            for entry in type_errors_jsonl(&type_errors) {
                println!("{entry}");
            }
            if let BuildCodegenStatus::Failed { phase, message } = &codegen_status {
                emit_jsonl_event(
                    "codegen_error",
                    &format!(
                        "\"phase\":{},\"message\":{}",
                        json_string(phase),
                        json_string(message),
                    ),
                );
            }
            if let BuildCodegenStatus::Built {
                exe_path,
                glue_path,
                dts_path,
                threads_wasm_path,
            } = &codegen_status
            {
                let mut fields = format!(
                    "\"output\":{}",
                    json_string(&exe_path.display().to_string())
                );
                if let Some(tw) = threads_wasm_path {
                    fields.push_str(&format!(
                        ",\"threads_wasm\":{}",
                        json_string(&tw.display().to_string())
                    ));
                }
                if let Some(js) = glue_path {
                    fields.push_str(&format!(
                        ",\"glue\":{}",
                        json_string(&js.display().to_string())
                    ));
                }
                if let Some(dts) = dts_path {
                    fields.push_str(&format!(
                        ",\"dts\":{}",
                        json_string(&dts.display().to_string())
                    ));
                }
                emit_jsonl_event("build_artifact", &fields);
            }
            emit_jsonl_event(
                "build_complete",
                &format!(
                    "\"success\":{},\"total_errors\":{}",
                    !failed,
                    parse_errors.iter().map(|pe| pe.errors.len()).sum::<usize>()
                        + cycles.len()
                        + resolve_errors
                            .iter()
                            .map(|re| re.errors.len())
                            .sum::<usize>()
                        + type_errors.iter().map(|te| te.errors.len()).sum::<usize>()
                        + codegen_status.error_count(),
                ),
            );
            if failed {
                process::exit(1);
            }
        }
    }
}

/// Result of the Theme 4 multi-file codegen pass appended to
/// [`cmd_build_project`]. Each variant maps to a downstream output mode
/// (text "Built: ..." line / JSON `"output"` field / JSONL
/// `build_artifact` event). `Built` and `Failed` are only constructed
/// under `cfg(feature = "llvm")` since the codegen pass itself is gated
/// on the same feature.
#[cfg_attr(not(feature = "llvm"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(super) enum BuildCodegenStatus {
    /// Earlier per-module phases failed (parse / cycles / resolve /
    /// typecheck), so codegen never ran. Output modes don't emit anything
    /// extra in this case — the per-phase diagnostics carry the failure.
    Skipped,
    /// `karac` was built without the `llvm` feature; project type-checks
    /// but no executable can be produced. Mirrors the single-file
    /// `cmd_build` no-llvm branch.
    NoLlvmFeature,
    /// All phases succeeded; the linked artifact is at `exe_path` (a
    /// native executable, or `dist/wasm/<pkg>.wasm` on a wasm target —
    /// under embedded component bindings that single file IS the
    /// componentized output). Browser-bindings WASM builds additionally
    /// carry the companion ES-module glue (`<pkg>.js`) and TypeScript
    /// declarations (`<pkg>.d.ts`) — each `None` on every other build
    /// shape.
    /// `--features wasm-threads` builds also carry the threaded module
    /// (`<pkg>.threads.wasm` — the dual artifact's second leg); `None`
    /// otherwise.
    Built {
        exe_path: PathBuf,
        glue_path: Option<PathBuf>,
        dts_path: Option<PathBuf>,
        threads_wasm_path: Option<PathBuf>,
    },
    /// Late-phase failure (effect / ownership / concurrency / codegen /
    /// link). `phase` names the failing phase for the diagnostic output;
    /// `message` is the rendered error.
    Failed { phase: String, message: String },
}

impl BuildCodegenStatus {
    fn error_count(&self) -> usize {
        match self {
            BuildCodegenStatus::Failed { .. } => 1,
            _ => 0,
        }
    }
}

/// Drive the multi-file codegen path: concatenate all module items into a
/// single super-program (in topological order, dropping `import`
/// declarations and the synthetic prelude), run the post-typecheck
/// pipeline (lower / effect / ownership / concurrency), then codegen +
/// link. Caller has already verified parse / cycles / resolve / typecheck
/// passed; this function returns a structured status the caller renders
/// per output mode.
///
/// **Multi-module diagnostics.** Late-phase diagnostics (effect /
/// ownership / concurrency / codegen / link) for the merged super-
/// program recover file-of-origin context via a `SpanLookupKey →
/// module_index` table built at concat time and consulted by
/// `format_pipeline_errors`. When a span resolves to exactly one
/// module the diagnostic is prefixed with `file:line:col`; when the
/// span is absent (e.g., synthesized post-concat) or ambiguous
/// (collision across modules — rare in practice but possible when
/// Reject a codegen build under `panic = "unwind"` (phase-8
/// `panic = "unwind" | "abort"` slice 2). v1's backend is abort-only — it never
/// emits the invoke / personality / landingpad machinery an unwinding panic
/// needs (the `unwind` codegen is the v1.x-gated slice 9), so a build that
/// selects `Unwind` cannot be honored and must fail loudly rather than silently
/// produce abort-semantics behavior under an unwind-labelled build. Only an
/// *explicit* `[profile] panic = "unwind"` reaches here: every profile defaults
/// to `Abort` at v1 ([`crate::manifest::ProfileConfig::panic_strategy`]), so a
/// normal build (no key, or `panic = "abort"`) passes. `Ok(())` on `Abort`.
#[cfg(feature = "llvm")]
pub(super) fn reject_unsupported_panic_strategy(
    strategy: crate::manifest::PanicStrategy,
) -> Result<(), String> {
    match strategy {
        crate::manifest::PanicStrategy::Abort => Ok(()),
        crate::manifest::PanicStrategy::Unwind => Err(
            "`panic = \"unwind\"` is not supported by the v1 backend (abort-only). \
             The unwinding-panic codegen (invoke / landingpad) is a v1.x feature; \
             remove the `[profile] panic` key or set `panic = \"abort\"`."
                .to_string(),
        ),
    }
}

/// two distinct files have identical leading bytes), the formatter
/// falls back to the file-less `line:col` form. Per-file
/// diagnostics for parse / cycles / resolve / typecheck still fire
/// upstream of this call.
#[cfg(feature = "llvm")]
#[allow(clippy::too_many_arguments)]
pub(super) fn run_multi_file_codegen(
    tree: &ProgramTree,
    mf: &crate::manifest::Manifest,
    project_root: &std::path::Path,
    enable_hot_swap: bool,
    release: bool,
    is_wasm: bool,
    effective_bindings: Option<BindingsMode>,
    wasm_tools: Option<&crate::componentize::WasmTools>,
    wasm_threads: bool,
    crate_type: NativeCrateType,
    out_path: Option<&str>,
) -> BuildCodegenStatus {
    // 1. Topological emission order — dependencies before dependents.
    let order = module::emission_order(tree);

    // 2. Concatenate items. Drop `import` declarations (their effect was
    // resolved upstream by per-module resolve) and skip synthetic
    // modules. Items keep their original spans, which downstream
    // diagnostics use for line:col reporting.
    //
    // While concatenating, build a `ModuleSpanTable`: for each non-
    // synthetic module we register its file path once, then walk every
    // appended item's spans so late-phase diagnostics can recover the
    // file-of-origin via `format_pipeline_errors`.
    let mut super_items: Vec<Item> = Vec::new();
    let mut span_table = crate::span_visitor::ModuleSpanTable::new();
    for &id in &order {
        let m = &tree.modules[id];
        if m.is_synthetic {
            continue;
        }
        let module_idx = span_table.register_module(m.file.clone());
        // Dropping the `import` declarations erases every ALIAS binding with
        // them — `import doer.{Impl as Widget};` leaves `Widget` naming
        // nothing in the flat unit, so the flat resolve/typecheck reject a
        // program the tree-aware per-module passes accepted
        // (B-2026-07-29-14). Canonicalize this module's references to the
        // imported items' real names first. A module with no aliased import
        // gets an empty substitution and is copied byte-identically.
        let mut items: Vec<Item> = m
            .items
            .iter()
            .filter(|it| !matches!(it, Item::Import(_)))
            .cloned()
            .collect();
        let local_names = crate::import_alias::declared_names(&items);
        let bound_values = crate::import_alias::bound_value_names(&mut items);
        let alias_subst =
            crate::import_alias::alias_subst_for_module(&m.imports, &local_names, &bound_values);
        for item in &mut items {
            crate::import_alias::rewrite_item(item, &alias_subst);
        }
        for item in items {
            span_table.record_item(module_idx, &item);
            super_items.push(item);
        }
    }

    // Phase-10 (`std.web`): gated baked stdlib modules are synthetic, so
    // the loop above never carries their items — an imported `fetch`
    // resolves and typechecks per-module (those passes chase the tree)
    // but its body would be missing here. Append the expansion of every
    // gated import found in user modules, deduplicated on the bound name
    // so two files importing the same item don't define it twice.
    {
        let mut seen: std::collections::HashSet<(Vec<String>, String)> =
            std::collections::HashSet::new();
        for m in &tree.modules {
            if m.is_synthetic {
                continue;
            }
            for imp in &m.imports {
                let deduped: Vec<crate::ast::ImportItem> = imp
                    .items
                    .iter()
                    .filter(|ii| {
                        let bound = ii.alias.as_ref().unwrap_or(&ii.name);
                        seen.insert((imp.path.clone(), bound.clone()))
                    })
                    .cloned()
                    .collect();
                if let Some(expansion) = crate::prelude::gated_items_for_import(&imp.path, &deduped)
                {
                    super_items.extend(expansion);
                }
            }
        }
    }

    let super_program = Program {
        items: super_items,
        ..Program::default()
    };

    // 3. Drive the rest of the pipeline by hand-constructing a Pipeline
    // with the synthetic ParseResult. This mirrors what `Pipeline::new`
    // would do on a single-file source, except we skip the parse step
    // entirely (we have a pre-built Program already).
    let parsed = ParseResult {
        program: super_program,
        errors: Vec::new(),
        fix_edits: FxHashMap::default(),
        fix_diffs: FxHashMap::default(),
    };
    let mut pipeline = Pipeline {
        filename: mf.name.clone(),
        target_skipped: std::collections::HashMap::new(),
        // No single source text: `parsed` is a super-program stitched from
        // every module. `filename` is the package NAME, not a path, so it must
        // never be read from disk (B-2026-08-04-7).
        source: None,
        parsed,
        // Project mode reaches this only for codegen-bound commands
        // (`build`/`run` on a package); the interpreter-bound single-file path
        // sets it through `Pipeline::interpreter_bound`.
        codegen_bound: true,
        resolved: None,
        typed: None,
        effects: None,
        ownership: None,
        concurrency: None,
        provider_escape: None,
        raii_errors: None,
        simd_errors: None,
        comptime_errors: None,
        profile: crate::manifest::CompileProfile::Default,
        profile_config: crate::manifest::ProfileConfig::default(),
        lint_overrides: crate::lints::CliLintOverrides::default(),
    };
    pipeline.resolve();
    if pipeline.has_resolve_errors() {
        return BuildCodegenStatus::Failed {
            phase: "resolve".to_string(),
            message: format_pipeline_errors(&pipeline, "resolve", Some(&span_table)),
        };
    }
    pipeline.typecheck();
    if pipeline
        .typed
        .as_ref()
        .is_some_and(|t| !t.errors.is_empty())
    {
        return BuildCodegenStatus::Failed {
            phase: "typecheck".to_string(),
            message: format_pipeline_errors(&pipeline, "typecheck", Some(&span_table)),
        };
    }
    pipeline.lower();
    pipeline.effectcheck();
    if pipeline
        .effects
        .as_ref()
        .is_some_and(|e| !e.errors.is_empty())
    {
        return BuildCodegenStatus::Failed {
            phase: "effect".to_string(),
            message: format_pipeline_errors(&pipeline, "effect", Some(&span_table)),
        };
    }
    pipeline.ownershipcheck();
    if pipeline
        .ownership
        .as_ref()
        .is_some_and(|o| !o.errors.is_empty())
    {
        return BuildCodegenStatus::Failed {
            phase: "ownership".to_string(),
            message: format_pipeline_errors(&pipeline, "ownership", Some(&span_table)),
        };
    }
    pipeline.concurrencycheck();
    if pipeline.has_fatal_errors() {
        return BuildCodegenStatus::Failed {
            phase: "checks".to_string(),
            message: format_pipeline_errors(&pipeline, "checks", Some(&span_table)),
        };
    }

    // Library-artifact C-ABI honesty gate (additive-interop Slice 2,
    // project-mode `[lib]`): reject a non-transparent, non-boxable export
    // return/param before codegen so the produced `.a`/`.so`/`.h` is always
    // ABI-honest (single-file posture; see `cmd_build`).
    if crate_type != NativeCrateType::Bin {
        let export_errs = crate::cheader::validate_exports(&pipeline.parsed.program);
        if !export_errs.is_empty() {
            let message = export_errs
                .iter()
                .map(|(fn_name, reason)| format!("exported `{fn_name}`: {reason}"))
                .collect::<Vec<_>>()
                .join("\n");
            return BuildCodegenStatus::Failed {
                phase: "export-abi".to_string(),
                message,
            };
        }
    }

    // 4. Codegen — write to a temp object then link to the manifest's
    // `name` field as the binary basename in the project root. A wasm
    // build instead lands in the `dist/wasm/<pkg>.wasm` artifact layout
    // (phase-10 WASM artifact emission; `link_executable` dispatches to
    // wasm-ld off the active target, same as single-file mode).
    let exe_path = if is_wasm {
        let dist = project_root.join("dist").join("wasm");
        if let Err(e) = std::fs::create_dir_all(&dist) {
            return BuildCodegenStatus::Failed {
                phase: "link".to_string(),
                message: format!("cannot create {}: {e}", dist.display()),
            };
        }
        dist.join(format!("{}.wasm", mf.name))
    } else {
        project_root.join(&mf.name)
    };
    let obj_path = std::env::temp_dir().join(format!(
        "karac_proj_{}_{}.o",
        std::process::id(),
        mf.name.replace(['/', '\\'], "_"),
    ));

    // phase-8 `panic = "unwind" | "abort"` slice 2: the v1 backend is
    // abort-only. Reject an explicit `[profile] panic = "unwind"` (from this
    // package's manifest) before codegen. Sourced from `mf` directly — the
    // manifest is authoritative for a project build.
    if let Err(e) = reject_unsupported_panic_strategy(mf.profile_config.panic_strategy()) {
        return BuildCodegenStatus::Failed {
            phase: "codegen".to_string(),
            message: e,
        };
    }

    if let Err(e) = crate::codegen::compile_to_object_with_hot_swap(
        &pipeline.parsed.program,
        &obj_path.to_string_lossy(),
        pipeline.ownership.as_ref(),
        // WASM concurrency lowering is its own phase-10 entry — until it
        // lands, suppress auto-par groups on wasm so modules lower
        // sequentially instead of emitting spawn-site calls into a
        // runtime archive with no scheduler (the single-file posture).
        if is_wasm {
            None
        } else {
            pipeline.concurrency.as_ref()
        },
        None,
        None,
        enable_hot_swap,
        // `--release` strips debug-only contract machinery in project mode,
        // same as single-file. OR-composes with `KARAC_STRIP_CONTRACTS`
        // (which still applies via the `Codegen::new` default when `release`
        // is false).
        release,
        true, // A2: coroutines on for project builds (bug-C fix reaches real builds)
    ) {
        let _ = std::fs::remove_file(&obj_path);
        return BuildCodegenStatus::Failed {
            phase: "codegen".to_string(),
            message: format!("codegen failed: {e}"),
        };
    }

    // Library-artifact producer mode (additive-interop Slice 2, project-
    // mode `[lib]`): archive/link the emitted object into a `.a`/`.so`/
    // `.dylib` under `dist/` and emit the companion `.h`, instead of an
    // executable. Native-only (the wasm × library combination was rejected
    // in `cmd_build_project`). Returns early — the wasm/exe link tail below
    // is `Bin`-only.
    if crate_type != NativeCrateType::Bin {
        let lib_kind = match crate_type {
            NativeCrateType::StaticLib => crate::codegen::NativeLibKind::StaticLib,
            NativeCrateType::CDylib => crate::codegen::NativeLibKind::CDylib,
            NativeCrateType::Bin => unreachable!(),
        };
        let lib_name = mf.lib_name.as_deref().unwrap_or(&mf.name);
        let dist = project_root.join("dist");
        if let Err(e) = std::fs::create_dir_all(&dist) {
            let _ = std::fs::remove_file(&obj_path);
            return BuildCodegenStatus::Failed {
                phase: "link".to_string(),
                message: format!("cannot create {}: {e}", dist.display()),
            };
        }
        let art_path = out_path.map(std::path::PathBuf::from).unwrap_or_else(|| {
            dist.join(format!("lib{lib_name}{}", lib_kind.artifact_extension()))
        });
        let export_syms = crate::cheader::export_symbols(&pipeline.parsed.program);
        if let Err(e) = crate::codegen::link_native_library(
            &obj_path.to_string_lossy(),
            &art_path.to_string_lossy(),
            lib_kind,
            lib_name,
            &export_syms,
        ) {
            let _ = std::fs::remove_file(&obj_path);
            return BuildCodegenStatus::Failed {
                phase: "link".to_string(),
                message: format!("link failed: {e}"),
            };
        }
        let _ = std::fs::remove_file(&obj_path);
        let header_path = art_path
            .parent()
            .map(|d| d.join(format!("lib{lib_name}.h")))
            .unwrap_or_else(|| std::path::PathBuf::from(format!("lib{lib_name}.h")));
        let header = crate::cheader::emit_c_header(&pipeline.parsed.program, lib_name);
        if let Err(e) = std::fs::write(&header_path, header) {
            return BuildCodegenStatus::Failed {
                phase: "link".to_string(),
                message: format!(
                    "library `{}` built, but writing the C header to {} failed: {e}",
                    art_path.display(),
                    header_path.display()
                ),
            };
        }
        print_staticlib_rust_host_note(crate_type);
        return BuildCodegenStatus::Built {
            exe_path: art_path,
            glue_path: None,
            dts_path: None,
            threads_wasm_path: None,
        };
    }

    // For embedded component bindings, wasm-ld's output is an
    // intermediate — link the C-ABI core module to a scratch path, then
    // lift it into the single component at `dist/wasm/<pkg>.wasm`. The
    // scratch basename is package-derived (not pid-bearing) so the module
    // name wasm-ld embeds — and the component carries — is reproducible
    // across rebuilds (B-2026-06-22-3); the dir carries the uniqueness.
    let (link_scratch_dir, link_out) = if wasm_tools.is_some() {
        match crate::componentize::link_core_scratch(&mf.name) {
            Ok((dir, core)) => (Some(dir), core),
            Err(e) => {
                let _ = std::fs::remove_file(&obj_path);
                return BuildCodegenStatus::Failed {
                    phase: "link".to_string(),
                    message: e,
                };
            }
        }
    } else {
        (None, exe_path.clone())
    };
    let wasm_export_names =
        crate::wasm_exports::link_export_names(&crate::wasm_exports::collect_wasm_exports(
            &pipeline.parsed.program,
            crate::target::active_target(),
        ));
    if let Err(e) = crate::codegen::link_executable_exports(
        &obj_path.to_string_lossy(),
        &link_out.to_string_lossy(),
        &wasm_export_names,
    ) {
        let _ = std::fs::remove_file(&obj_path);
        return BuildCodegenStatus::Failed {
            phase: "link".to_string(),
            message: format!("link failed: {e}"),
        };
    }
    let _ = std::fs::remove_file(&obj_path);
    if let Some(tool) = wasm_tools {
        let host_fns = crate::wasm_glue::collect_host_fns(&pipeline.parsed.program);
        let wasm_exports = crate::wasm_exports::collect_wasm_exports(
            &pipeline.parsed.program,
            crate::target::active_target(),
        );
        warn_unlowered_exports(
            &wasm_exports,
            crate::wasm_exports::ExportSig::component_lowerable,
        );
        let result = crate::componentize::componentize(
            tool,
            &link_out,
            &host_fns,
            &wasm_exports,
            &mf.name,
            &exe_path,
        );
        if let Some(dir) = &link_scratch_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        if let Err(e) = result {
            return BuildCodegenStatus::Failed {
                phase: "componentize".to_string(),
                message: format!("componentize failed: {e}"),
            };
        }
    }

    // `--features wasm-threads`: the dual artifact's second pass — same
    // front-end output, auto-par re-enabled, wasip1-threads machine,
    // --shared-memory link. Lands as `dist/wasm/<pkg>.threads.wasm`
    // next to the sequential module; knobs come straight from the
    // project's own manifest (already loaded — no walk-up).
    let (threads_wasm_path, threads_glue_cfg) = if wasm_threads {
        let threads_filename = format!("{}.threads.wasm", mf.name);
        let threads_path = exe_path.with_file_name(&threads_filename);
        let threads_obj = std::env::temp_dir().join(format!(
            "karac_proj_{}_{}.threads.o",
            std::process::id(),
            mf.name.replace(['/', '\\'], "_"),
        ));
        let cfg = emit_wasm_threads_artifact(
            &pipeline.parsed.program,
            pipeline.ownership.as_ref(),
            pipeline.concurrency.as_ref(),
            None,
            None,
            release,
            &threads_obj.to_string_lossy(),
            &threads_path,
            &threads_filename,
            (
                mf.wasm_pool_size,
                mf.wasm_fallback,
                mf.wasm_max_memory_pages,
            ),
        );
        (Some(threads_path), Some(cfg))
    } else {
        (None, None)
    };

    // Companion artifacts next to the module in `dist/wasm/`, keyed on
    // the resolved bindings mode — exactly the single-file `cmd_build`
    // contract: browser bindings ship the ES-module glue + TypeScript
    // declarations (`<pkg>.js` / `<pkg>.d.ts`, see `wasm_glue`);
    // embedded component bindings ship NO companion (`<pkg>.wasm` IS
    // the self-describing component).
    let mut glue_path = None;
    let mut dts_path = None;
    match effective_bindings {
        Some(BindingsMode::Browser) => {
            let host_fns = crate::wasm_glue::collect_host_fns(&pipeline.parsed.program);
            let wasm_exports = crate::wasm_exports::collect_wasm_exports(
                &pipeline.parsed.program,
                crate::target::active_target(),
            );
            warn_unlowered_exports(
                &wasm_exports,
                crate::wasm_exports::ExportSig::component_lowerable,
            );
            let wasm_filename = format!("{}.wasm", mf.name);
            let js = exe_path.with_extension("js");
            if let Err(e) = std::fs::write(
                &js,
                crate::wasm_glue::render_glue(
                    &host_fns,
                    &wasm_exports,
                    &wasm_filename,
                    threads_glue_cfg.as_ref(),
                ),
            ) {
                return BuildCodegenStatus::Failed {
                    phase: "link".to_string(),
                    message: format!("failed to write JS glue {}: {e}", js.display()),
                };
            }
            glue_path = Some(js);
            let dts = exe_path.with_extension("d.ts");
            if let Err(e) = std::fs::write(
                &dts,
                crate::wasm_glue::render_dts(
                    &host_fns,
                    &wasm_exports,
                    &wasm_filename,
                    threads_glue_cfg.is_some(),
                ),
            ) {
                return BuildCodegenStatus::Failed {
                    phase: "link".to_string(),
                    message: format!("failed to write TS declarations {}: {e}", dts.display()),
                };
            }
            dts_path = Some(dts);
        }
        Some(BindingsMode::Component) | Some(BindingsMode::None) | None => {}
    }
    BuildCodegenStatus::Built {
        exe_path,
        glue_path,
        dts_path,
        threads_wasm_path,
    }
}

/// Stub for the no-llvm build — never invoked because the caller gates
/// on `cfg!(feature = "llvm")`. Kept as a parallel signature so the call
/// site doesn't need cfg gating itself.
#[cfg(not(feature = "llvm"))]
#[allow(clippy::too_many_arguments)]
pub(super) fn run_multi_file_codegen(
    _tree: &ProgramTree,
    _mf: &crate::manifest::Manifest,
    _project_root: &std::path::Path,
    _enable_hot_swap: bool,
    _release: bool,
    _is_wasm: bool,
    _effective_bindings: Option<BindingsMode>,
    _wasm_tools: Option<&crate::componentize::WasmTools>,
    _wasm_threads: bool,
    _crate_type: NativeCrateType,
    _out_path: Option<&str>,
) -> BuildCodegenStatus {
    BuildCodegenStatus::NoLlvmFeature
}

/// Render a structured error list across the post-typecheck pipeline
/// phases for the multi-file project-mode build path. `table` is the
/// per-module span lookup built at concat time in
/// `run_multi_file_codegen` — when present and the span resolves to
/// exactly one module, the diagnostic line is prefixed with
/// `file:line:col`; otherwise it falls back to bare `line:col` so
/// callers without a table (or with a span absent from the table /
/// shared across modules) still get a useful location.
#[cfg(feature = "llvm")]
pub(super) fn format_pipeline_errors(
    pipeline: &Pipeline,
    phase: &str,
    table: Option<&crate::span_visitor::ModuleSpanTable>,
) -> String {
    use std::fmt::Write;
    let mut out = format!("multi-file {phase} failed:");
    let prefix = |span: &crate::token::Span| -> String {
        if let Some(t) = table {
            if let Some(p) = t.lookup(span) {
                return format!("{}:", p.display());
            }
        }
        String::new()
    };
    match phase {
        "resolve" => {
            if let Some(r) = &pipeline.resolved {
                for e in &r.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&e.span),
                        e.span.line,
                        e.span.column,
                        e.message,
                    );
                }
            }
        }
        "typecheck" => {
            if let Some(t) = &pipeline.typed {
                for e in &t.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&e.span),
                        e.span.line,
                        e.span.column,
                        e.message,
                    );
                }
            }
        }
        "effect" => {
            if let Some(e) = &pipeline.effects {
                for err in &e.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&err.span),
                        err.span.line,
                        err.span.column,
                        err.message,
                    );
                }
            }
        }
        "ownership" => {
            if let Some(o) = &pipeline.ownership {
                for err in &o.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&err.span),
                        err.span.line,
                        err.span.column,
                        err.message,
                    );
                }
            }
        }
        // The "checks" branch is reached when `has_fatal_errors`
        // returns true after a late-phase pass; today that flag is
        // driven by parse + resolve errors only (concurrency analysis
        // emits structured decisions, not errors), but we surface
        // every accumulated error here so the user gets file-context
        // wherever a span is available rather than the generic
        // "late-phase analysis failed" stub.
        "checks" => {
            if let Some(r) = &pipeline.resolved {
                for e in &r.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&e.span),
                        e.span.line,
                        e.span.column,
                        e.message,
                    );
                }
            }
            if let Some(t) = &pipeline.typed {
                for e in &t.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&e.span),
                        e.span.line,
                        e.span.column,
                        e.message,
                    );
                }
            }
            if let Some(e) = &pipeline.effects {
                for err in &e.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&err.span),
                        err.span.line,
                        err.span.column,
                        err.message,
                    );
                }
            }
            if let Some(o) = &pipeline.ownership {
                for err in &o.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&err.span),
                        err.span.line,
                        err.span.column,
                        err.message,
                    );
                }
            }
        }
        _ => {}
    }
    out
}

pub(super) fn print_parse_errors_text(parse_errors: &[ModuleParseErrors]) {
    for pe in parse_errors {
        for err in &pe.errors {
            eprintln!(
                "error[parse]: {}:{}:{}: {}",
                pe.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
        }
    }
}

/// Resolver errors collected for one specific module, with the source file
/// retained so diagnostics can be printed with their original location.
pub(super) struct ModuleResolveErrors {
    file: PathBuf,
    errors: Vec<ResolveError>,
    /// Multi-edit fix envelopes for this module's diagnostics, keyed by the
    /// owning diagnostic's span. Carried alongside `errors` so the
    /// project-mode JSON path advertises the same fixes the single-file path
    /// does — see [`crate::resolver::ResolveResult::error_fix_diffs`].
    fix_diffs: FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
}

/// Run the resolver per module with the full `ProgramTree` attached so
/// cross-module imports can be validated. Returns only modules that produced
/// errors — a module with a clean resolve contributes nothing.
pub(super) fn resolve_modules(tree: &ProgramTree) -> Vec<ModuleResolveErrors> {
    let mut out = Vec::new();
    for (id, m) in tree.modules.iter().enumerate() {
        // Compiler-injected modules (CR-24 slice 8: `std.prelude` placeholder)
        // skip per-module passes — their stub items only exist to surface the
        // module path to cross-module resolution.
        if m.is_synthetic {
            continue;
        }
        // Resolver still takes a `&Program`, so wrap the module's items
        // in a freshly-owned `Program` view. Clone cost is negligible next
        // to the resolver pass itself.
        let program = Program {
            items: m.items.clone(),
            ..Program::default()
        };
        let result = Resolver::new(&program)
            .with_tree(tree, id as ModuleId)
            .with_test_file(m.is_test_file)
            .resolve();
        if !result.errors.is_empty() {
            out.push(ModuleResolveErrors {
                file: m.file.clone(),
                errors: result.errors,
                fix_diffs: result.error_fix_diffs,
            });
        }
    }
    out
}

pub(super) fn resolve_error_code(kind: &ResolveErrorKind) -> &'static str {
    match kind {
        ResolveErrorKind::UnknownModule => "E0112",
        ResolveErrorKind::UnknownItemInModule => "E0113",
        ResolveErrorKind::PrivateItemAccess => "E0111",
        ResolveErrorKind::UndefinedName => "E0100",
        ResolveErrorKind::DuplicateDefinition => "E0101",
        ResolveErrorKind::ReservedIdentifier => "E0102",
        ResolveErrorKind::PrivateAccess => "E0103",
        ResolveErrorKind::UndefinedType => "E0104",
        ResolveErrorKind::UndefinedVariant => "E0105",
        ResolveErrorKind::UndefinedField => "E0106",
        ResolveErrorKind::UndefinedLabel => "E0107",
        ResolveErrorKind::OperatorTraitImplRestricted => "E0108",
        ResolveErrorKind::IntoTraitImplNotAllowed => "E0109",
        ResolveErrorKind::ImplLevelEffectVarNotAllowed => "E0110",
        ResolveErrorKind::ReservedEffectResource => "E0114",
        ResolveErrorKind::CompilerBuiltinReserved => "E0115",
        ResolveErrorKind::ContinueOnBlockLabel => "E0116",
        ResolveErrorKind::NonExhaustiveInvalidTarget => "E0117",
        ResolveErrorKind::TrackCallerInvalidTarget => "E0118",
        ResolveErrorKind::GpuInvalidTarget => "E0800",
        ResolveErrorKind::CodegenHintInvalidTarget => "E_CODEGEN_HINT_INVALID_POSITION",
        ResolveErrorKind::CodegenHintOnExternDecl => "E_CODEGEN_HINT_ON_EXTERN_DECL",
        ResolveErrorKind::DeprecatedOnImpl => "E0119",
        ResolveErrorKind::DeprecatedOnField => "E0120",
        ResolveErrorKind::UnknownAttribute => "E0121",
        ResolveErrorKind::ProfileInvalidTarget => "E0122",
        ResolveErrorKind::UnknownProfile => "E0123",
        ResolveErrorKind::QueryResolutionConflict => "E_QUERY_RESOLUTION_CONFLICT",
        ResolveErrorKind::UnionNonExhaustiveForbidden => "E_UNION_NON_EXHAUSTIVE_FORBIDDEN",
        ResolveErrorKind::DefaultAttributeInvalidPosition => "E_DEFAULT_ATTRIBUTE_INVALID_POSITION",
        ResolveErrorKind::DefaultAttributeWithoutDerive => "E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE",
        ResolveErrorKind::MalformedAttributeArgs => "E_MALFORMED_ATTRIBUTE_ARGS",
    }
}

pub(super) fn print_resolve_errors_text(per_module: &[ModuleResolveErrors]) {
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            eprintln!(
                "error[{code}]: {}:{}:{}: {}",
                re.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
            if let Some(ref s) = err.suggestion {
                eprintln!("  help: did you mean `{s}`?");
            }
            if let Some(ref stub) = err.stub_hint {
                let target_file = sibling_production_file(&file);
                eprintln!(
                    "  hint: stub `{}` in {} with inferred signature:",
                    stub.callee_name, target_file
                );
                for line in stub.render_source().lines() {
                    eprintln!("    {line}");
                }
            }
        }
    }
}

/// Render `err.replacement` as `,"replacement":{...}` JSON tail (or empty
/// string when no replacement is attached). Mirrors the single-file
/// `print_diagnostics_json` path at the top of this file so IDE quick-fix
/// consumers see the same payload regardless of how `karac check` was
/// invoked. Multi-file-only diagnostics (E0112 / E0113) reach IDEs only
/// through this path.
pub(super) fn replacement_json_tail(err: &crate::resolver::ResolveError) -> String {
    match err.replacement.as_deref() {
        Some(r) => format!(
            ",\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
            r.offset,
            r.length,
            json_string(&r.replacement),
        ),
        None => String::new(),
    }
}

/// Render a multi-edit fix envelope as a `,"fix_diff":[{...}]` JSON tail
/// (empty when the diagnostic has none). The project-mode twin of the
/// single-file `fix_diff` emission in `collect_diagnostics`; without it a
/// diagnostic whose fix spans several edits — today
/// `E_MODULE_BINDING_NAMING`'s rename — would look unfixable to an IDE
/// simply because the project was compiled as a project.
pub(super) fn fix_diff_json_tail(
    err: &crate::resolver::ResolveError,
    fix_diffs: &FxHashMap<crate::resolver::SpanKey, Vec<crate::resolver::TextEdit>>,
) -> String {
    match fix_diffs
        .get(&crate::resolver::SpanKey::from_span(&err.span))
        .filter(|v| !v.is_empty())
    {
        Some(edits) => {
            let items: Vec<String> = edits
                .iter()
                .map(|e| {
                    format!(
                        "{{\"offset\":{},\"length\":{},\"text\":{}}}",
                        e.offset,
                        e.length,
                        json_string(&e.replacement),
                    )
                })
                .collect();
            format!(",\"fix_diff\":[{}]", items.join(","))
        }
        None => String::new(),
    }
}

pub(super) fn resolve_errors_json(per_module: &[ModuleResolveErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            let suggestion = match err.suggestion.as_deref() {
                Some(s) => format!(",\"suggestion\":{}", json_string(s)),
                None => String::new(),
            };
            let replacement = replacement_json_tail(err);
            let fix_diff = fix_diff_json_tail(err, &re.fix_diffs);
            let hints = stub_hints_tail(&file, err);
            out.push(format!(
                "{{\"severity\":\"error\",\"phase\":\"resolve\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}{}{}{}{}}}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                suggestion,
                replacement,
                fix_diff,
                hints,
            ));
        }
    }
    out
}

pub(super) fn resolve_errors_jsonl(per_module: &[ModuleResolveErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            let suggestion = match err.suggestion.as_deref() {
                Some(s) => format!(",\"suggestion\":{}", json_string(s)),
                None => String::new(),
            };
            let replacement = replacement_json_tail(err);
            let fix_diff = fix_diff_json_tail(err, &re.fix_diffs);
            let hints = stub_hints_tail(&file, err);
            out.push(format!(
                "{{\"type\":\"resolve_error\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}{}{}{}{}}}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                suggestion,
                replacement,
                fix_diff,
                hints,
            ));
        }
    }
    out
}

/// Emit the `,"hints":[…]` JSON tail when `err` carries a stub hint —
/// the multi-module resolve-error emitters' counterpart to the
/// `hints[].diff` wiring inside `DiagnosticJson::add`. Returns the
/// empty string when no stub hint is present so the JSON shape stays
/// lean for the common case.
pub(super) fn stub_hints_tail(file: &str, err: &crate::resolver::ResolveError) -> String {
    match err.stub_hint.as_ref() {
        Some(s) => format!(",\"hints\":[{}]", render_stub_hint_json(file, s)),
        None => String::new(),
    }
}

/// Typechecker errors collected for one specific module.
pub(super) struct ModuleTypeErrors {
    file: PathBuf,
    errors: Vec<crate::typechecker::TypeError>,
}

/// Run the typechecker per module with the full `ProgramTree` attached so
/// the CR-24 slice-6 cross-module `E0221` + field-access rules can fire.
/// A fresh resolver pass per module provides the `ResolveResult` the
/// typechecker still consumes internally.
/// Per-module typecheck output: the errors that gate the build, and the
/// WARNINGS that do not.
///
/// The two are separated rather than merged because `type_errors.is_empty()`
/// is the project build's gate — folding warning-carrying modules into that
/// vector would abort a build over a `#[deprecated]` call. B-2026-08-18-19.
pub(super) struct ModuleTypeDiagnostics {
    pub(super) errors: Vec<ModuleTypeErrors>,
    /// Same per-file shape, carrying `TypeCheckResult::warnings` — the channel
    /// `deprecated` / `unstable_api` ride.
    pub(super) warnings: Vec<ModuleTypeErrors>,
}

pub(super) fn typecheck_modules(
    tree: &ProgramTree,
    lint_overrides: &crate::lints::CliLintOverrides,
) -> ModuleTypeDiagnostics {
    let mut out = Vec::new();
    let mut warn_out = Vec::new();
    for (id, m) in tree.modules.iter().enumerate() {
        // Skip the compiler-injected `std.prelude` placeholder — its stubs
        // would clash with `register_builtin_types` if pushed through the
        // typechecker's normal item-collection.
        if m.is_synthetic {
            continue;
        }
        let program = Program {
            items: m.items.clone(),
            ..Program::default()
        };
        let resolved = Resolver::new(&program)
            .with_tree(tree, id as ModuleId)
            .resolve();
        let result = crate::typechecker::TypeChecker::new(&program, &resolved)
            .with_tree(tree, id as ModuleId)
            .with_cli_lint_overrides(lint_overrides.clone())
            .check();
        // B-2026-08-18-19 — the warnings used to be DROPPED here: `result`
        // carries them and only `.errors` was read, so a project build was
        // structurally incapable of reporting `deprecated` no matter what the
        // render path did.
        if !result.warnings.is_empty() {
            warn_out.push(ModuleTypeErrors {
                file: m.file.clone(),
                errors: result.warnings,
            });
        }
        if !result.errors.is_empty() {
            out.push(ModuleTypeErrors {
                file: m.file.clone(),
                errors: result.errors,
            });
        }
    }
    ModuleTypeDiagnostics {
        errors: out,
        warnings: warn_out,
    }
}

pub(super) fn type_error_code(kind: &crate::typechecker::TypeErrorKind) -> &'static str {
    use crate::typechecker::TypeErrorKind as K;
    match kind {
        K::PrivateTypeInPublicSignature => "E0221",
        K::TypeMismatch => "E0200",
        K::UndefinedField => "E0201",
        K::WrongNumberOfArgs => "E0202",
        K::MissingField => "E0203",
        K::ExtraField => "E0204",
        K::NonExhaustiveMatch => "E0205",
        K::NotCallable => "E0206",
        K::NotAStruct => "E0207",
        K::InvalidBinaryOp => "E0208",
        K::InvalidUnaryOp => "E0209",
        K::InvalidCast => "E0210",
        K::ConditionNotBool => "E0211",
        K::BranchTypeMismatch => "E0212",
        K::ReturnTypeMismatch => "E0213",
        K::GpuNotSafe => "E0801",
        K::StringNotIndexable => "E0268",
        K::IteratorNotIndexable => "E0274",
        K::TypeNotIndexable => "E0275",
        K::NilCoalesceNotWrapped => "E0276",
        K::OptionalChainNotOption => "E0277",
        K::SharedFieldNotMut => "E0269",
        K::AtomicMissingOrdering => "E0270",
        K::AtomicInvalidInnerType => "E0272",
        K::PatternScrutineeMismatch => "E0273",
        _ => "E0200",
    }
}

/// The warning-severity twin of [`print_type_errors_text`]. Same layout, with
/// the severity word and the lint label a `-A <name>` invocation would take —
/// matching what `karac check` prints for the same finding.
pub(super) fn print_type_warnings_text(per_module: &[ModuleTypeErrors]) {
    for te in per_module {
        for warn in &te.errors {
            let label = warn.lint_name.as_deref().unwrap_or("typecheck");
            eprintln!(
                "warning[{label}]: {}:{}:{}: {}",
                te.file.display(),
                warn.span.line,
                warn.span.column,
                warn.message,
            );
        }
    }
}

pub(super) fn print_type_errors_text(per_module: &[ModuleTypeErrors]) {
    for te in per_module {
        for err in &te.errors {
            // A promoted lint names its lint rather than the generic type
            // code — see the single-file renderer's note (B-2026-08-18-25).
            // `-D deprecated` rendered `error[E0200]` here, the TypeMismatch
            // code, which points at neither the rule nor the flag.
            let code = err
                .lint_name
                .clone()
                .unwrap_or_else(|| type_error_code(&err.kind).to_string());
            eprintln!(
                "error[{code}]: {}:{}:{}: {}",
                te.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
        }
    }
}

/// The warning-severity twin of [`type_errors_json`]. Carries `lint_name` so a
/// consumer can offer the `-A <name>` the human renderer names, matching what
/// `karac check --output=json` emits for the same finding.
pub(super) fn type_warnings_json(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = te.file.display().to_string();
        for warn in &te.errors {
            let mut record = format!(
                "{{\"severity\":\"warning\",\"phase\":\"typecheck\",\"file\":{},\"line\":{},\"column\":{},\"message\":{}",
                json_string(&file),
                warn.span.line,
                warn.span.column,
                json_string(&warn.message),
            );
            if let Some(lint) = warn.lint_name.as_deref() {
                record.push_str(&format!(",\"lint_name\":{}", json_string(lint)));
            }
            record.push('}');
            out.push(record);
        }
    }
    out
}

pub(super) fn type_errors_json(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = te.file.display().to_string();
        for err in &te.errors {
            let code = type_error_code(&err.kind);
            let mut record = format!(
                "{{\"severity\":\"error\",\"phase\":\"typecheck\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{},\"class\":{}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                json_string(err.class.map(|c| c.as_str()).unwrap_or("OTHER")),
            );
            if let Some(expected) = &err.expected {
                record.push_str(&format!(",\"expected\":{}", json_string(expected)));
            }
            if let Some(got) = &err.got {
                record.push_str(&format!(",\"got\":{}", json_string(got)));
            }
            record.push('}');
            out.push(record);
        }
    }
    out
}

pub(super) fn type_errors_jsonl(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = te.file.display().to_string();
        for err in &te.errors {
            let code = type_error_code(&err.kind);
            let mut record = format!(
                "{{\"type\":\"type_error\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{},\"class\":{}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                json_string(err.class.map(|c| c.as_str()).unwrap_or("OTHER")),
            );
            if let Some(expected) = &err.expected {
                record.push_str(&format!(",\"expected\":{}", json_string(expected)));
            }
            if let Some(got) = &err.got {
                record.push_str(&format!(",\"got\":{}", json_string(got)));
            }
            record.push('}');
            out.push(record);
        }
    }
    out
}

/// The module-aware diagnostics for a WHOLE package: the same walk → tree →
/// cycles → per-module resolve → per-module typecheck sequence `cmd_build_project`
/// runs, without any codegen.
///
/// Extracted so `karac check` and `karac run` can reach the answer `karac build`
/// already had (B-2026-08-20-16). Before this, those two commands saw only the
/// ONE file named on the command line, which on a package member is neither the
/// program the user wrote nor the program that gets built: an `import` of a
/// sibling module resolves to nothing, so a `pub struct` read through it draws
/// an invented "`X` is not a struct", while a `private` item imported across
/// directories draws no E0111 at all. Both directions were wrong, and
/// `karac check --output=json` is the Mend loop's diagnostics feed.
pub(super) struct PackageCheck {
    pub(super) tree: ProgramTree,
    pub(super) parse_errors: Vec<crate::module::ModuleParseErrors>,
    pub(super) cycles: Vec<Cycle>,
    pub(super) resolve_errors: Vec<ModuleResolveErrors>,
    pub(super) type_errors: Vec<ModuleTypeErrors>,
    pub(super) type_warnings: Vec<ModuleTypeErrors>,
}

impl PackageCheck {
    /// Any error-severity diagnostic, in any module. Warnings are excluded —
    /// they do not gate an exit status.
    pub(super) fn has_errors(&self) -> bool {
        !self.parse_errors.is_empty()
            || !self.cycles.is_empty()
            || self.resolve_errors.iter().any(|m| !m.errors.is_empty())
            || self.type_errors.iter().any(|m| !m.errors.is_empty())
    }

    /// How many error-severity diagnostics this holds, across every module it
    /// still carries. Used for the caller's summary line, so it counts what was
    /// RENDERED — call it after [`Self::restrict_to_file`], not before.
    pub(super) fn error_count(&self) -> usize {
        self.parse_errors
            .iter()
            .map(|m| m.errors.len())
            .sum::<usize>()
            + self.cycles.len()
            + self
                .resolve_errors
                .iter()
                .map(|m| m.errors.len())
                .sum::<usize>()
            + self
                .type_errors
                .iter()
                .map(|m| m.errors.len())
                .sum::<usize>()
    }

    /// Drop every per-module diagnostic that does not belong to `file`, and
    /// return how many ERRORS were dropped.
    ///
    /// `karac check <file>` asks about one file and should answer about that
    /// file — reporting the whole package would flood a caller that named a
    /// single module, and `--output=json` consumers reasonably expect the
    /// diagnostics to concern what they asked about. The dropped count is not
    /// discarded: the caller prints a one-line pointer at `karac check` so a
    /// sibling module's real error is never silently swallowed.
    ///
    /// Cycles are NOT filtered — a cycle is a property of the graph, not of one
    /// file, and it invalidates this file's resolution too.
    pub(super) fn restrict_to_file(&mut self, file: &std::path::Path) -> usize {
        let keep = |p: &std::path::Path| -> bool {
            // Compare canonically where both sides canonicalize; fall back to a
            // literal match so a path that no longer exists still filters
            // predictably rather than dropping everything.
            match (std::fs::canonicalize(p), std::fs::canonicalize(file)) {
                (Ok(a), Ok(b)) => a == b,
                _ => p == file,
            }
        };
        let mut dropped = 0usize;
        dropped += self
            .parse_errors
            .iter()
            .filter(|m| !keep(&m.file))
            .map(|m| m.errors.len())
            .sum::<usize>();
        self.parse_errors.retain(|m| keep(&m.file));
        dropped += self
            .resolve_errors
            .iter()
            .filter(|m| !keep(&m.file))
            .map(|m| m.errors.len())
            .sum::<usize>();
        self.resolve_errors.retain(|m| keep(&m.file));
        dropped += self
            .type_errors
            .iter()
            .filter(|m| !keep(&m.file))
            .map(|m| m.errors.len())
            .sum::<usize>();
        self.type_errors.retain(|m| keep(&m.file));
        self.type_warnings.retain(|m| keep(&m.file));
        dropped
    }
}

/// Run [`PackageCheck`] over the package rooted at `root`.
///
/// `Err` carries an already-formatted message for the walk/tree failures that
/// have no per-file span (a broken manifest, a mixed `main.kara`/`lib.kara`
/// entry pair). Callers on the check/run paths treat those as fatal, matching
/// `karac build`.
pub(super) fn package_check(
    root: &std::path::Path,
    lint_overrides: &crate::lints::CliLintOverrides,
) -> Result<PackageCheck, String> {
    let walked = walker::walk_project(root, WalkerOpts::default()).map_err(|e| format!("{e}"))?;
    // Same lenient dep posture as the `karac run` super-program builder: a
    // dep-resolution failure proceeds without dependency modules rather than
    // failing the check of the local package.
    let dep_walks = super::run_check_cmds::quiet_dep_package_walks(root);
    let built =
        module::build_program_tree_with_deps(&walked, &dep_walks, module::BuildTreeOpts::default())
            .map_err(|e| format!("{e}"))?;
    let BuildTreeOk { tree, parse_errors } = built;
    let cycles = module::detect_cycles(&tree);
    // Ordering mirrors `cmd_build_project` exactly, including the skips: a
    // half-parsed or cyclic tree cascades spurious E0112/E0113s, and a
    // half-resolved one cascades type errors. Answering differently here would
    // reintroduce the very divergence this function exists to remove.
    let resolve_errors = if parse_errors.is_empty() && cycles.is_empty() {
        resolve_modules(&tree)
    } else {
        Vec::new()
    };
    let diags = if parse_errors.is_empty() && cycles.is_empty() && resolve_errors.is_empty() {
        typecheck_modules(&tree, lint_overrides)
    } else {
        ModuleTypeDiagnostics {
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    };
    Ok(PackageCheck {
        tree,
        parse_errors,
        cycles,
        resolve_errors,
        type_errors: diags.errors,
        type_warnings: diags.warnings,
    })
}

pub(super) fn print_cycles_text(cycles: &[Cycle], tree: &ProgramTree) {
    for c in cycles {
        eprintln!("error[E0223]: circular module dependency");
        eprintln!("  cycle: {}", c.format(tree));
        // Name the FILE behind each module on the cycle (B-2026-08-20-19). A
        // cycle is the one module diagnostic with no single span to point at —
        // the offending edge is spread across several files — so the closest
        // thing to "where do I go and look" is the list of files involved. The
        // JSON emitter has carried `cycle_files` all along; the text render
        // left the reader to map dotted module paths back to paths on disk.
        for &id in &c.nodes {
            let m = &tree.modules[id];
            let label = if m.path.is_empty() {
                "<crate root>".to_string()
            } else {
                m.path.join(".")
            };
            eprintln!("    {label}  {}", m.file.display());
        }
        eprintln!(
            "  suggestion: extract the shared items into a lower-layer module that both ends of the cycle can depend on."
        );
    }
}

pub(super) fn parse_errors_json(parse_errors: &[ModuleParseErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for pe in parse_errors {
        let file = pe.file.display().to_string();
        for err in &pe.errors {
            out.push(format!(
                "{{\"severity\":\"error\",\"phase\":\"parse\",\"code\":\"E0001\",\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
            ));
        }
    }
    out
}

pub(super) fn cycles_json(cycles: &[Cycle], tree: &ProgramTree) -> Vec<String> {
    cycles
        .iter()
        .map(|c| {
            let paths: Vec<String> = c
                .nodes
                .iter()
                .map(|id| {
                    let p = &tree.modules[*id].path;
                    if p.is_empty() {
                        String::new()
                    } else {
                        p.join(".")
                    }
                })
                .collect();
            let paths_json: Vec<String> = paths.iter().map(|s| json_string(s)).collect();
            let files: Vec<String> = c
                .nodes
                .iter()
                .map(|id| json_string(&tree.modules[*id].file.display().to_string()))
                .collect();
            format!(
                "{{\"severity\":\"error\",\"phase\":\"module_graph\",\"code\":\"E0223\",\"message\":{},\"cycle_paths\":[{}],\"cycle_files\":[{}]}}",
                json_string(&c.format(tree)),
                paths_json.join(","),
                files.join(","),
            )
        })
        .collect()
}

pub(super) fn parse_errors_jsonl(parse_errors: &[ModuleParseErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for pe in parse_errors {
        let file = pe.file.display().to_string();
        for err in &pe.errors {
            out.push(format!(
                "{{\"type\":\"parse_error\",\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
            ));
        }
    }
    out
}

pub(super) fn cycles_jsonl(cycles: &[Cycle], tree: &ProgramTree) -> Vec<String> {
    cycles
        .iter()
        .map(|c| {
            let paths: Vec<String> = c
                .nodes
                .iter()
                .map(|id| {
                    let p = &tree.modules[*id].path;
                    if p.is_empty() {
                        String::new()
                    } else {
                        p.join(".")
                    }
                })
                .collect();
            let paths_json: Vec<String> = paths.iter().map(|s| json_string(s)).collect();
            format!(
                "{{\"type\":\"module_cycle\",\"code\":\"E0223\",\"message\":{},\"cycle_paths\":[{}]}}",
                json_string(&c.format(tree)),
                paths_json.join(","),
            )
        })
        .collect()
}

pub(super) fn emit_build_tree_error(e: &BuildTreeError, output: OutputMode) {
    let code = e.code().unwrap_or("module");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"module_graph\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "build_tree_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}

pub(super) fn entry_label(entry: EntryKind) -> &'static str {
    match entry {
        EntryKind::Bin => "bin",
        EntryKind::Lib => "lib",
        EntryKind::None => "none",
    }
}

pub(super) fn render_walked_modules_json(walked: &WalkResult) -> String {
    walked
        .modules
        .iter()
        .map(|m| {
            let path = if m.path.is_empty() {
                String::new()
            } else {
                m.path.join(".")
            };
            let role = match m.role {
                walker::ModuleRole::Ordinary => "ordinary",
                walker::ModuleRole::Entry => "entry",
                walker::ModuleRole::Test => "test",
            };
            let platform = match m.platform {
                Some(p) => json_string(p.as_suffix()),
                None => "null".to_string(),
            };
            format!(
                "{{\"path\":{},\"role\":{},\"platform\":{},\"file\":{}}}",
                json_string(&path),
                json_string(role),
                platform,
                json_string(&m.file.display().to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn emit_manifest_error(e: &manifest::ManifestError, output: OutputMode) {
    let code = e.code().unwrap_or("manifest");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"manifest\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "manifest_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}

/// The active target triple for resolver invocations that carry no `--target`
/// flag — `test` / `resolve` / `update`. Precedence mirrors the `None`-flag
/// case of `cmd_build_project`'s target resolution: the manifest's
/// `[build].default-target` if set, else the host triple. Threading this
/// through `merge_target_overlay` (below) makes these commands consume the
/// same per-target `[dependencies]` view a plain `karac build` would — the
/// resolver-follow-up-(e) gap where only `cmd_build_project` merged the
/// overlay, so `[target.<triple>.dependencies]` silently dropped out of
/// `test` / `resolve` / `update`.
pub(super) fn default_resolution_target(mf: &manifest::Manifest) -> String {
    mf.build_default_target
        .clone()
        .unwrap_or_else(crate::build_cache::host_target_triple)
}

/// Surface dependency-resolution diagnostics for the static/lenient commands
/// that consult a project but don't fetch — `karac check` and `karac run`
/// (resolver follow-up (m)). **Path-dep-only** — both are fast passes that must
/// not touch the network, so no registry/git provider is threaded in. Returns
/// `false` (halt the command) when a fatal graph error is emitted; `true` to
/// continue.
///
/// A structural graph error — a dependency cycle, a missing path-dep, or a
/// workspace-deref failure — fails `build_dep_graph_with_options` and halts.
/// A version conflict or an MSRV (`kara-version`) violation surfaces from the
/// resolve step and halts. Registry/git deps, by contrast, cannot be satisfied
/// without a fetch and neither command fetches or loads dependency modules from
/// a registry/git source, so an `E_*_DEP_UNSUPPORTED` finding is a build-time
/// concern — it is skipped here (the same dep surfaces on `karac build`).
pub(super) fn surface_dep_graph_diagnostics(
    root: &std::path::Path,
    mf: crate::manifest::Manifest,
    output: OutputMode,
) -> bool {
    let loader = crate::dep_graph::FsLoader;
    let options = crate::dep_graph::DepGraphOptions {
        offline_root: None,
        include_dev_deps: false,
        registry_provider: None,
        git_provider: None,
        // Path-dep-only lenient walk — no lockfile pinning here.
        pins: None,
    };
    let graph = match crate::dep_graph::build_dep_graph_with_options(root, mf, &loader, options) {
        Ok(g) => g,
        Err(e) => {
            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
            emit_dep_diagnostic(&diag, output, "error");
            return false;
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::dep_resolver::resolve(&graph, &active) {
        Ok(_) => true,
        Err(boxed) => {
            if matches!(
                boxed.code(),
                "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED"
            ) {
                return true;
            }
            let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
            emit_dep_diagnostic(&diag, output, "error");
            false
        }
    }
}

/// Build the dep graph and resolve it against the active toolchain. Returns
/// `true` to continue with the build, `false` to halt. Registry/git
/// unsupported errors downgrade to warnings — the rest are fatal. Slice 7
/// of the PubGrub-resolver entry (`docs/implementation_checklist/phase-5-
/// diagnostics.md` line 813). Wiring point: `cmd_build_project` right
/// after the manifest loads.
///
/// `include_dev_deps` activates the test-mode walk (line 884) — the root
/// manifest's `[dev-dependencies]` participate in resolution. Off in
/// build mode; on in test mode. Dev-deps do not propagate through
/// transitive children regardless of the flag.
/// Resolve the project's dependency graph. `Err(())` means a fatal
/// diagnostic was emitted and the build must halt. `Ok(Some(resolution))`
/// carries the concrete package set for cross-package module loading
/// (phase-5 line 898); `Ok(None)` is the legacy warning-and-continue path
/// (unsupported registry/git sources outside offline mode), where the
/// build proceeds without dependency modules.
pub(super) fn run_dep_resolution(
    root: &std::path::Path,
    mf: crate::manifest::Manifest,
    output: OutputMode,
    offline_root: Option<&std::path::Path>,
    include_dev_deps: bool,
    no_proxy: bool,
    persist_lock: bool,
) -> Result<Option<crate::dep_resolver::Resolution>, ()> {
    let loader = crate::dep_graph::FsLoader;

    // Activate the registry fetch path. A `ProxyRegistryProvider` — the
    // cache → retry → live-HTTP decorator stack — is threaded into the graph
    // walk so a `[dependencies]` registry entry is fetched, extracted, and
    // recursed into exactly like a path-dep. The *only* difference between
    // the proxy path (slice 4) and the direct-from-source path (follow-ups
    // (j)/(k)) is which base URL the HTTP client points at; the whole stack
    // below (retry + tarball cache + extraction) is base-URL-agnostic.
    //
    // Decide the effective registry base URL, if any:
    //   * `--offline` / no usable cache root: never touch the network — the
    //     `vendor/` walk owns resolution — so no base URL, provider stays off.
    //   * `--no-proxy` (direct-from-source, follow-ups (j)/(k)): fetch straight
    //     from the configured upstream registry (`KARAC_REGISTRY_URL` /
    //     `[build].registry`), bypassing the proxy. Unconfigured → `None`, so a
    //     registry dep keeps the warn-and-continue contract
    //     (`E_REGISTRY_DEP_UNSUPPORTED`) rather than fetching against nothing.
    //   * otherwise (proxy path): fetch through the configured proxy. The
    //     built-in `DEFAULT_PROXY_URL` is a not-yet-live placeholder, so this
    //     only activates once an operator points `KARAC_REGISTRY_PROXY` (or
    //     `[build].registry-proxy`) at a real proxy (`explicit_proxy_configured`).
    //
    // The client stack and provider live in locals whose borrows outlive the
    // `build_dep_graph_with_options` call below (it returns an owned graph
    // with every registry resolution already materialized to disk).
    let cache_root = crate::registry_proxy::default_registry_cache_root();
    let registry_base: Option<String> = if offline_root.is_some() || cache_root.is_none() {
        None
    } else if no_proxy {
        crate::registry_proxy::resolve_direct_registry_url(mf.build_registry.as_deref())
    } else if crate::registry_proxy::explicit_proxy_configured(mf.build_registry_proxy.as_deref()) {
        Some(
            crate::registry_proxy::ProxyConfig::resolve(
                crate::registry_proxy::ProxyMode::Default,
                mf.build_registry_proxy.as_deref(),
            )
            .url,
        )
    } else {
        None
    };
    let client_stack: Option<Box<dyn crate::registry_proxy::ProxyClient>> =
        registry_base.map(|url| {
            // A per-user `KARAC_REGISTRY_TOKEN` authenticates a private proxy
            // or private direct registry alike.
            let token = crate::registry_proxy::registry_token_from_env();
            let http = crate::registry_proxy::HttpProxyClient::with_token(url, token);
            let retrying = crate::registry_proxy::RetryingProxyClient::new(
                Box::new(http),
                crate::registry_proxy::RetryPolicy::default(),
            );
            // Tarball cache under <root>/<name>/<version>/package.tar.gz; the
            // provider extracts to the sibling <root>/<name>/<version>/src, so
            // the two share one root without colliding.
            let caching = crate::registry_proxy::CachingProxyClient::new(
                Box::new(retrying),
                cache_root.clone().unwrap_or_default(),
            );
            Box::new(caching) as Box<dyn crate::registry_proxy::ProxyClient>
        });
    let provider = client_stack.as_ref().map(|c| {
        crate::registry_extract::ProxyRegistryProvider::new(
            c.as_ref(),
            cache_root.clone().unwrap_or_default(),
        )
    });

    // Git deps are direct-from-source (no proxy in the loop), so git fetch is
    // gated only on `--offline` — not on `--no-proxy` or an explicitly
    // configured proxy. A git URL is real (unlike the placeholder default
    // proxy), so cloning whenever a git dep is declared is always correct.
    let git_provider = if offline_root.is_none() {
        crate::git_fetch::default_git_cache_root().map(crate::git_fetch::GitCliProvider::new)
    } else {
        None
    };

    // Lockfile-pin-over-catalog (follow-up (d)/(h)): read an existing
    // `kara.lock` and prefer its recorded registry-package versions, so a
    // rebuild reproduces the locked graph rather than drifting to the newest
    // compatible version. The pins feed BOTH the graph walk (slice 4 — a pinned
    // registry dep is fetched at exactly that version via `fetch_exact`, even if
    // since-yanked, and added to the candidate set) and version selection (slice
    // 2). Best-effort — an absent / unreadable / malformed lockfile yields no
    // pins (fresh selection), never a build error. Pins bite only where the
    // registry candidate set is widened; path/git deps ignore them.
    let pins = read_lockfile_pins(root);
    let options = crate::dep_graph::DepGraphOptions {
        offline_root,
        include_dev_deps,
        registry_provider: provider
            .as_ref()
            .map(|p| p as &dyn crate::dep_graph::RegistryProvider),
        git_provider: git_provider
            .as_ref()
            .map(|p| p as &dyn crate::git_fetch::GitProvider),
        pins: Some(&pins),
    };
    let graph = match crate::dep_graph::build_dep_graph_with_options(root, mf, &loader, options) {
        Ok(g) => g,
        Err(e) => {
            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
            emit_dep_diagnostic(&diag, output, "error");
            return Err(());
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::dep_resolver::resolve_with_pins(&graph, &active, offline_root, &pins) {
        Ok(resolution) => {
            // Warn on any resolved version the catalog marks yanked (follow-up
            // (h), slice 4). Fresh selection excludes yanked versions, so this
            // fires only when a `kara.lock` pin lands on a version yanked since
            // it was recorded — reproducibility kept it, and the user should
            // hear the pin is now withdrawn.
            emit_yanked_pin_warnings(&resolution, &graph, output);
            // `karac resolve` is read-only — it inspects the graph without
            // rewriting `kara.lock`. Only build / test persist the pin.
            if persist_lock {
                persist_lockfile(root, &resolution, output);
            }
            Ok(Some(resolution))
        }
        Err(boxed) => {
            let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
            let code = boxed.code();
            // In offline mode, registry/git deps can't be satisfied from
            // vendor/ today (registry/git vendoring lands alongside line
            // 845); the unsupported-source diagnostic must halt the build
            // so the operator doesn't get a silent partial resolution.
            // Outside offline, the existing warning-and-continue behavior
            // preserves the pre-fetch v1.1 contract.
            let severity = if offline_root.is_some() {
                "error"
            } else {
                match code {
                    "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED" => "warning",
                    _ => "error",
                }
            };
            emit_dep_diagnostic(&diag, output, severity);
            if severity == "warning" {
                Ok(None)
            } else {
                Err(())
            }
        }
    }
}

/// Walk each resolved path-dependency's source tree for cross-package
/// module loading (phase-5 line 898). Returns one [`module::DepPackageWalk`]
/// per path-sourced package, in `Resolution`'s deterministic (BTreeMap,
/// name-sorted) order. `Err(())` means a diagnostic was already emitted.
///
/// Dependencies must be library packages: a dep whose entry is
/// `src/main.kara` (or which has no entry at all) is a hard error, since
/// its items have nowhere to hoist and a binary cannot be imported.
/// Dependency test companions are excluded (`include_tests: false`) — a
/// consumer never compiles its deps' tests.
pub(super) fn dep_package_walks(
    resolution: Option<&crate::dep_resolver::Resolution>,
    target: walker::Platform,
    output: OutputMode,
) -> Result<Vec<module::DepPackageWalk>, ()> {
    let Some(resolution) = resolution else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for pkg in resolution.packages.values() {
        // Path-deps, fetched registry deps, and cloned git deps all carry an
        // on-disk source root the module loader compiles (each threaded its
        // materialized directory into its `ResolvedSource` variant). `Root`
        // is the project itself.
        let dep_root: &std::path::Path = match &pkg.source {
            crate::dep_resolver::ResolvedSource::Path(dir) => dir,
            crate::dep_resolver::ResolvedSource::Registry { dir, .. } => dir,
            crate::dep_resolver::ResolvedSource::Git { dir, .. } => dir,
            _ => continue,
        };
        let walk_opts = WalkerOpts {
            target,
            include_tests: false,
        };
        let walked = match walker::walk_project(dep_root, walk_opts) {
            Ok(w) => w,
            Err(e) => {
                emit_dep_walk_error(&pkg.name, &e.to_string(), output);
                return Err(());
            }
        };
        if walked.entry != walker::EntryKind::Lib {
            let why = match walked.entry {
                walker::EntryKind::Bin => {
                    "it has `src/main.kara` — a binary package cannot be imported"
                }
                _ => "it has no `src/lib.kara` entry file",
            };
            emit_dep_walk_error(
                &pkg.name,
                &format!(
                    "dependency `{}` is not a library package: {}",
                    pkg.name, why
                ),
                output,
            );
            return Err(());
        }
        out.push(module::DepPackageWalk {
            name: pkg.name.clone(),
            walked,
        });
    }
    Ok(out)
}

/// Render a dependency-walk failure (walker error or non-library dep) in
/// the active output mode. Mirrors `emit_walker_error`'s shape with the
/// owning package named in the message.
pub(super) fn emit_dep_walk_error(pkg: &str, message: &str, output: OutputMode) {
    match output {
        OutputMode::Text => {
            eprintln!("error[walker]: in dependency `{pkg}`: {message}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"walker\",\"code\":\"walker\",\"package\":{},\"message\":{}}}]}}",
                json_string(pkg),
                json_string(message),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "walker_error",
                &format!(
                    "\"code\":\"walker\",\"package\":{},\"message\":{}",
                    json_string(pkg),
                    json_string(message),
                ),
            );
        }
    }
}

/// `karac-toolchain.toml` enforcement (tracker line 892). Returns
/// `true` to continue with the build, `false` to halt. When the file
/// is absent the function is a no-op. When present, the declared
/// `version` constraint is intersected against the active compiler
/// version; mismatch surfaces `E_TOOLCHAIN_VERSION_MISMATCH` with a
/// `karaup` hint. Parse errors halt with the file-specific symbolic
/// code so the operator hears about a malformed pin rather than
/// silently building against an unintended toolchain.
pub(super) fn enforce_toolchain_pin(root: &std::path::Path, output: OutputMode) -> bool {
    let load = crate::karac_toolchain::load_from_start(root);
    let (path, spec) = match load {
        Ok(Some(pair)) => pair,
        Ok(None) => return true,
        Err(e) => {
            emit_toolchain_load_error(&e, output);
            return false;
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::karac_toolchain::enforce(&spec, &path, &active) {
        Ok(()) => true,
        Err(mismatch) => {
            emit_toolchain_mismatch(&mismatch, output);
            false
        }
    }
}

/// Render a `karac_toolchain::ToolchainError` (parse / IO failure) into
/// the active output mode. Symbolic code surfaces so downstream tooling
/// can recognize the kind of failure without parsing the message.
pub(super) fn emit_toolchain_load_error(
    err: &crate::karac_toolchain::ToolchainError,
    output: OutputMode,
) {
    let code = err.code();
    let primary = err.to_string();
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"toolchain_pin\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "toolchain_pin_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
}

/// Render a `karac_toolchain::ToolchainMismatch` diagnostic into the
/// active output mode. The note documents the v1 limitation: karac
/// today reads the pin but does not auto-switch — operators install
/// the required toolchain via `karaup` (deferred) or manually.
pub(super) fn emit_toolchain_mismatch(
    mismatch: &crate::karac_toolchain::ToolchainMismatch,
    output: OutputMode,
) {
    let code = mismatch.code();
    let primary = mismatch.message();
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            eprintln!("   = note: install a matching toolchain via `karaup install {}` (karaup ships post-v1)", mismatch.required);
            eprintln!("   = help: or relax the `version` constraint in `karac-toolchain.toml` to admit the active toolchain");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"toolchain_pin\",\"code\":{},\"message\":{},\"required\":{},\"active\":{}}}]}}",
                json_string(code),
                json_string(&primary),
                json_string(&mismatch.required.to_string()),
                json_string(&mismatch.active.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "toolchain_pin_error",
                &format!(
                    "\"code\":{},\"message\":{},\"required\":{},\"active\":{}",
                    json_string(code),
                    json_string(&primary),
                    json_string(&mismatch.required.to_string()),
                    json_string(&mismatch.active.to_string()),
                ),
            );
        }
    }
}

/// Pre-check diagnostic for `karac build --offline` when the project root
/// has no `./vendor/` directory. The resolver would otherwise error per-dep
/// with `E_OFFLINE_VENDOR_ENTRY_MISSING`; surfacing the missing root once,
/// up front, is a clearer operator hint.
pub(super) fn emit_offline_no_vendor_dir(vendor_dir: &std::path::Path, output: OutputMode) {
    let code = "E_OFFLINE_NO_VENDOR_DIR";
    let primary = format!(
        "offline build requires a vendor directory at `{}` but none was found",
        vendor_dir.display()
    );
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            eprintln!("   = note: --offline resolves every transitive path-dep against `./vendor/<name>/`");
            eprintln!("   = help: run `karac vendor` to populate the vendor directory, then re-run with `--offline`");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"dep_resolution\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "dep_resolution_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
}

/// Read the project's `kara.lock` (if present) and extract the registry-package
/// version pins for lockfile-pin-over-catalog resolution (follow-up (d)/(h),
/// slice 3). Best-effort: a missing / unreadable / malformed lockfile yields an
/// empty pin map, so a fresh project or a corrupt lockfile falls back to fresh
/// version selection rather than failing the build.
pub(super) fn read_lockfile_pins(
    root: &std::path::Path,
) -> std::collections::BTreeMap<String, semver::Version> {
    let path = root.join("kara.lock");
    let Ok(source) = std::fs::read_to_string(&path) else {
        return std::collections::BTreeMap::new();
    };
    match crate::lockfile::Lockfile::parse(&path, &source) {
        Ok(lock) => lock.version_pins(),
        Err(_) => std::collections::BTreeMap::new(),
    }
}

/// Emit a `W_DEPENDENCY_YANKED` warning for each resolved package whose selected
/// version the catalog marks yanked (resolver follow-up (h), slice 4). Fresh
/// selection never picks a yanked version, so this only fires when a `kara.lock`
/// pin lands on a version yanked *since* it was recorded — reproducibility kept
/// the pin, and the user should hear it is now withdrawn. Purely advisory: it
/// never fails the build.
pub(super) fn emit_yanked_pin_warnings(
    resolution: &crate::dep_resolver::Resolution,
    graph: &crate::dep_graph::DepGraph,
    output: OutputMode,
) {
    for pkg in resolution.packages.values() {
        let Some(yanked) = graph.yanked_versions.get(&pkg.name) else {
            continue;
        };
        if !yanked.contains(&pkg.version) {
            continue;
        }
        let diag = crate::dep_diagnostic::Diagnostic {
            code: "W_DEPENDENCY_YANKED",
            primary: format!(
                "dependency `{}` is pinned to version {}, which the registry has yanked",
                pkg.name, pkg.version
            ),
            notes: vec![
                "the version is recorded in `kara.lock`, but the registry has since withdrawn it — a yanked release is kept resolvable for reproducibility, yet should not be relied on for new work".to_string(),
            ],
            help: Some(format!(
                "run `karac update {}` to move to a non-yanked version, or pin a different version in `kara.toml`",
                pkg.name
            )),
        };
        emit_dep_diagnostic(&diag, output, "warning");
    }
}

/// Slice 4 of the lockfile entry (phase-5 line 831). Materializes a fresh
/// `kara.lock` from the resolver's output and writes it at the project root.
/// On read-then-rewrite paths, suppresses the write when the bytes are
/// identical so file mtimes are stable across no-op rebuilds. Any lockfile
/// IO failure is emitted as a warning (build-blocking would be too strict
/// in v1.1 — the resolver already succeeded; lockfile drift is recoverable
/// on the next build). Errors mid-build don't fail the build.
pub(super) fn persist_lockfile(
    root: &std::path::Path,
    resolution: &crate::dep_resolver::Resolution,
    output: OutputMode,
) {
    let lockfile = match crate::lockfile::Lockfile::from_resolution(
        resolution,
        root,
        crate::lockfile::compute_path_dep_hash,
    ) {
        Ok(lf) => lf,
        Err(e) => {
            emit_lockfile_warning(&e, output);
            return;
        }
    };

    let lockfile_path = root.join("kara.lock");
    let fresh_toml = lockfile.to_toml();

    // No-op-when-unchanged: avoid touching file mtime on a quiet rebuild.
    if let Ok(existing) = std::fs::read_to_string(&lockfile_path) {
        if existing == fresh_toml {
            return;
        }
    }

    if let Err(io) = std::fs::write(&lockfile_path, &fresh_toml) {
        let err = crate::lockfile::LockfileError::Io {
            path: lockfile_path,
            error: io.to_string(),
        };
        emit_lockfile_warning(&err, output);
    }
}

pub(super) fn emit_lockfile_warning(err: &crate::lockfile::LockfileError, output: OutputMode) {
    let primary = err.to_string();
    let code = err.code();
    match output {
        OutputMode::Text => {
            eprintln!("warning[{code}]: {primary}");
            eprintln!("   = note: the resolver succeeded; the lockfile write is a follow-up step");
            eprintln!("   = help: check filesystem permissions for the project root");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"ok\",\"diagnostics\":[{{\"severity\":\"warning\",\"phase\":\"lockfile\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "lockfile_warning",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
}

pub(super) fn emit_dep_diagnostic(
    diag: &crate::dep_diagnostic::Diagnostic,
    output: OutputMode,
    severity: &str,
) {
    match output {
        OutputMode::Text => {
            eprintln!(
                "{}[{}]: {}",
                if severity == "warning" {
                    "warning"
                } else {
                    "error"
                },
                diag.code,
                diag.primary,
            );
            for note in &diag.notes {
                eprintln!("   = note: {note}");
            }
            if let Some(help) = &diag.help {
                eprintln!("   = help: {help}");
            }
        }
        OutputMode::Json => {
            let notes_json = diag
                .notes
                .iter()
                .map(|n| json_string(n))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"status\":\"{}\",\"diagnostics\":[{{\"severity\":\"{}\",\"phase\":\"dep_resolution\",\"code\":{},\"message\":{},\"notes\":[{}]{}}}]}}",
                if severity == "warning" { "ok" } else { "error" },
                severity,
                json_string(diag.code),
                json_string(&diag.primary),
                notes_json,
                diag.help
                    .as_ref()
                    .map(|h| format!(",\"help\":{}", json_string(h)))
                    .unwrap_or_default(),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                &format!("dep_resolution_{severity}"),
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(diag.code),
                    json_string(&diag.primary),
                ),
            );
        }
    }
}

/// Emit a one-line confirmation `note:` when `--no-proxy` is set. The note
/// reports both the active proxy URL (so the operator sees what `karac`
/// would have consulted) and a pointer at the v1.1.x registry-proxy
/// follow-up. Silent when `--no-proxy` is absent — the proxy is the
/// default and the existing registry-dep-unsupported warning carries the
/// status. Emitted on the first cmd_* entry point so it is consistent
/// across `build`, `update`, and `vendor`.
pub(super) fn emit_no_proxy_note(no_proxy: bool) {
    if !no_proxy {
        return;
    }
    // Best-effort: if we're in a project, honor its `[build]` pins so the
    // reported URLs match what a fetch would consult. Outside a project (or on
    // a malformed manifest) fall through to env/default.
    let manifest = std::env::current_dir()
        .ok()
        .and_then(|cwd| manifest::load_from_cwd(&cwd).ok())
        .map(|(_, mf)| mf);
    let manifest_proxy = manifest
        .as_ref()
        .and_then(|mf| mf.build_registry_proxy.clone());
    let manifest_registry = manifest.as_ref().and_then(|mf| mf.build_registry.clone());
    let config = crate::registry_proxy::ProxyConfig::resolve(
        crate::registry_proxy::ProxyMode::Disabled,
        manifest_proxy.as_deref(),
    );
    // When a direct upstream registry is configured (env or `[build].registry`),
    // `--no-proxy` fetches direct-from-source (follow-ups (j)/(k)) rather than
    // warn-and-continue; name the registry so the operator sees where deps come
    // from. Otherwise keep the pre-fetch note.
    match crate::registry_proxy::resolve_direct_registry_url(manifest_registry.as_deref()) {
        Some(registry_url) => eprintln!(
            "note: --no-proxy active; registry deps fetch direct-from-source at {registry_url} (proxy at {} bypassed)",
            config.url
        ),
        None => eprintln!(
            "note: --no-proxy active; registry deps will not consult the proxy at {} (set KARAC_REGISTRY_URL or [build].registry to fetch direct-from-source)",
            config.url
        ),
    }
}

pub(super) fn emit_walker_error(e: &walker::WalkerError, output: OutputMode) {
    let code = e.code().unwrap_or("walker");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"walker\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "walker_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}
