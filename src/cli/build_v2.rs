//! `karac build` on v2 (M3 item 4, CLI half, B-2026-10-22-14): the program is
//! checked as `karac check` checks it, built to MIR and compiled by mir-llvm
//! into a native executable. A package with several modules is built as one
//! source (project mode, src/cli/project_v2.rs), from a member file or, with
//! no file named, from the package in the current directory. A flag v2 does not
//! honour yet, or one whose feature went to the archive with the legacy
//! pipeline at M3, is refused by name.

use super::*;

/// The build flags, as `karac build` parsed them.
pub(super) struct BuildArgs<'a> {
    pub output: OutputMode,
    pub offline: bool,
    pub no_proxy: bool,
    pub concurrency_report: bool,
    pub simd_report: bool,
    pub enable_hot_swap: bool,
    pub target: Option<&'a str>,
    pub bindings: Option<BindingsMode>,
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    pub target_cpu: Option<&'a str>,
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    pub target_features: Option<&'a str>,
    pub wasm_threads: bool,
    pub monomorphization_budget: bool,
    pub release: bool,
    pub crate_type: NativeCrateType,
    pub out_path: Option<&'a str>,
    pub lint_overrides: crate::lints::CliLintOverrides,
}

/// `karac build <file>` on v2.
pub(super) fn cmd_build_v2_file(filename: &str, a: BuildArgs) {
    refuse_unsupported(&a);
    let output = a.output;
    let manifest = run_manifest_preamble(filename, output, None, false);
    #[cfg(feature = "llvm")]
    if !set_wasm_target(&a) {
        apply_cpu(&a, |pick| {
            manifest_release_field_for(filename, output, pick)
        });
    }
    // The `[link]` table's libraries, for the program's `extern "C"`
    // functions (B-2026-10-22-10).
    super::args::install_manifest_link(filename);
    let project = super::run_v2::package_source(filename, false, output, "build");
    let stem = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main");
    let exe = a
        .out_path
        .map_or_else(|| format!("{stem}{}", exe_suffix(&a)), str::to_string);
    build(filename, project, manifest.as_ref(), a, &exe);
}

/// `karac build` with no file on v2: the package in the current directory,
/// from its `src/main.kara`, into `<root>/<package name>` (or `-o`).
pub(super) fn cmd_build_v2_project(a: BuildArgs) {
    refuse_unsupported(&a);
    let output = a.output;
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        fail_with(
            &format!("error: cannot read current directory: {e}"),
            output,
        );
        process::exit(1);
    });
    let (root, raw) = manifest::load_from_cwd(&cwd).unwrap_or_else(|e| {
        emit_manifest_error(&e, output);
        process::exit(1);
    });
    if !super::build_cmds::enforce_toolchain_pin(&root, output) {
        process::exit(1);
    }
    let host = crate::build_cache::host_target_triple();
    let mf = manifest::merge_target_overlay(&raw, Some(host.as_str()));
    if output == OutputMode::Text {
        for w in &mf.warnings {
            eprintln!("warning[manifest]: {}", w.message);
        }
    }
    let entry = root.join("src").join("main.kara");
    if !entry.is_file() {
        fail_with(
            &format!(
                "error: the v2 build makes an executable from src/main.kara, and {} has none; \
                 v2 does not build a library package yet",
                root.display()
            ),
            output,
        );
        process::exit(1);
    }
    #[cfg(feature = "llvm")]
    if !set_wasm_target(&a) {
        apply_cpu(&a, |pick| pick(&mf));
    }
    if !mf.link_libs.is_empty() || !mf.link_search_paths.is_empty() {
        crate::target::set_native_link_config(mf.link_libs.clone(), mf.link_search_paths.clone());
    }
    // The dependencies, fetched and pinned in kara.lock as the legacy build
    // does: `--offline` reads them from `vendor/`, `--no-proxy` fetches a
    // registry dependency from its registry directly.
    if !a.offline {
        emit_no_proxy_note(a.no_proxy);
    }
    let has_deps =
        !mf.dependencies.is_empty() || !mf.dev_dependencies.is_empty() || mf.kara_version.is_some();
    let vendor = root.join("vendor");
    if a.offline && has_deps && !vendor.is_dir() {
        emit_offline_no_vendor_dir(&vendor, output);
        process::exit(1);
    }
    let resolution = if has_deps {
        let offline_root = a.offline.then_some(vendor.as_path());
        run_dep_resolution(
            &root,
            mf.clone(),
            output,
            offline_root,
            false,
            a.no_proxy,
            true,
        )
        .unwrap_or_else(|()| process::exit(1))
    } else {
        None
    };
    let walk = WalkerOpts::default();
    let walked = walker::walk_project(&root, walk).unwrap_or_else(|e| {
        emit_walker_error(&e, output);
        process::exit(1);
    });
    let deps = dep_package_walks(resolution.as_ref(), walk.target, output)
        .unwrap_or_else(|()| process::exit(1));
    // The package-wide check walks these too, the fetched ones included.
    install_resolved_dep_walks(deps.clone());
    let tree =
        module::build_program_tree_with_deps(&walked, &deps, module::BuildTreeOpts::default())
            .unwrap_or_else(|e| {
                emit_build_tree_error(&e, output);
                process::exit(1);
            });
    let filename = super::project_v2::shown(&entry);
    // One module is a lone file; anything else (a parse error included,
    // which the package check reports) is the package.
    let project = (!tree.parse_errors.is_empty()
        || tree.tree.modules.iter().filter(|m| !m.is_synthetic).count() > 1)
        .then(|| super::run_v2::combine(&root, &tree.tree, output, "build"));
    let exe = a.out_path.map_or_else(
        || {
            let name = format!("{}{}", mf.name, exe_suffix(&a));
            root.join(name).display().to_string()
        },
        str::to_string,
    );
    build(&filename, project, Some(&mf), a, &exe);
}

/// `--target=wasm_wasi`: a plain wasm32-wasip1 module, linked against
/// `libkarac_runtime_wasm.a` (M3 item 4, B-2026-10-22-23). The component
/// model (`--bindings`) and `wasm_browser` are not on v2 yet.
fn is_wasm_wasi(a: &BuildArgs) -> bool {
    a.target == Some("wasm_wasi")
}

/// The output's extension when `-o` does not name it.
fn exe_suffix(a: &BuildArgs) -> &'static str {
    if is_wasm_wasi(a) {
        ".wasm"
    } else {
        ""
    }
}

/// Makes wasm32-wasip1 the active target for `--target=wasm_wasi`; true
/// when it did (the native CPU baseline then does not apply).
#[cfg(feature = "llvm")]
fn set_wasm_target(a: &BuildArgs) -> bool {
    if !is_wasm_wasi(a) {
        return false;
    }
    if let Err(e) = crate::target::set_active_target("wasm_wasi") {
        fail_with(&e, a.output);
        process::exit(1);
    }
    true
}

/// A flag the v2 build does not honour yet: refused by name, exit 1.
fn refuse_unsupported(a: &BuildArgs) {
    let target = a.target.filter(|t| {
        *t != "native"
            && *t != "wasm_wasi"
            && (crate::target::is_v1_target_name(t)
                || *t != crate::build_cache::host_target_triple())
    });
    // B-2026-10-22-21: a feature that went to the archive with the legacy
    // backend is refused by name rather than as an unknown flag.
    let archived_target = match target {
        Some("wasm_browser") => Some("the browser wasm bindings"),
        Some("gpu") => Some("GPU compute"),
        _ => None,
    };
    let refused: Vec<String> = [
        (a.concurrency_report, "--concurrency-report".to_string()),
        (a.simd_report, "--simd-report".to_string()),
        (a.enable_hot_swap, "--enable-hot-swap".to_string()),
        (
            target.is_some() && archived_target.is_none(),
            format!("--target={}", target.unwrap_or("")),
        ),
        (
            a.monomorphization_budget,
            "--monomorphization-budget".to_string(),
        ),
    ]
    .into_iter()
    .filter_map(|(on, flag)| on.then_some(format!("`{flag}`")))
    .collect();
    let archived: Vec<(String, &str)> = [
        (
            archived_target.is_some(),
            format!("--target={}", target.unwrap_or("")),
            archived_target.unwrap_or(""),
        ),
        (
            // `--bindings=none` asks for the raw core module, which is what
            // the v2 `wasm_wasi` build emits.
            a.bindings
                .is_some_and(|b| !(b == super::BindingsMode::None && is_wasm_wasi(a))),
            "--bindings".to_string(),
            "the wasm component bindings",
        ),
        (
            a.wasm_threads,
            "--features=wasm-threads".to_string(),
            "threaded wasm",
        ),
        (
            a.crate_type != NativeCrateType::Bin,
            "--crate-type".to_string(),
            "the exported C ABI",
        ),
    ]
    .into_iter()
    .filter_map(|(on, flag, feature)| on.then_some((format!("`{flag}`"), feature)))
    .collect();
    let mut lines = Vec::new();
    if !refused.is_empty() {
        lines.push(format!(
            "error: the v2 build does not honour {} yet",
            refused.join(", ")
        ));
    }
    for (flag, feature) in archived {
        lines.push(format!(
            "error: {flag} builds {feature}, which goes to the archive with the legacy \
             backend at M3 and returns on v2 on demand"
        ));
    }
    if !lines.is_empty() {
        fail_with(&lines.join("\n"), a.output);
        process::exit(1);
    }
    if std::env::var("KARAC_AUTO_PAR").is_ok_and(|v| v == "1") {
        eprintln!(
            "warning: `KARAC_AUTO_PAR=1` is ignored: auto-parallelisation goes to the \
             archive with the legacy backend at M3 and returns on v2 in M4"
        );
    }
}

/// The CPU baseline, as `karac build` sets it: the flag, then
/// `KARAC_TARGET_CPU`, then the manifest's `[release] target-cpu`, then its
/// `cpu-baseline` (`v3` when absent); the feature string likewise.
#[cfg(feature = "llvm")]
fn apply_cpu(
    a: &BuildArgs,
    manifest_field: impl Fn(fn(&manifest::Manifest) -> Option<String>) -> Option<String>,
) {
    if a.target_cpu == Some("help") || a.target_features == Some("help") {
        crate::backend::driver::print_target_cpu_listing();
        process::exit(0);
    }
    let (baseline_cpu, baseline_features) =
        resolve_native_cpu_baseline(manifest_field(|m| m.release_cpu_baseline.clone()).as_deref());
    apply_target_cpu_override(
        a.target_cpu
            .map(str::to_string)
            .or_else(read_target_cpu_env)
            .or_else(|| manifest_field(|m| m.release_target_cpu.clone()))
            .or(baseline_cpu),
    );
    apply_target_features_override(
        a.target_features
            .map(str::to_string)
            .or_else(read_target_features_env)
            .or_else(|| manifest_field(|m| m.release_target_features.clone()))
            .or(baseline_features),
    );
}

/// Check, build and link the program into `exe`. `KARAC_MIR_OPT=<0..3>` sets
/// the IR optimisation level (2 by default); `KARAC_MIR_LL=<path>` keeps a
/// copy of the IR.
fn build(
    filename: &str,
    project: Option<super::project_v2::Amalgam>,
    manifest: Option<&manifest::Manifest>,
    a: BuildArgs,
    exe: &str,
) {
    let output = a.output;
    if a.release {
        // `dbg()` and contracts are stripped from release builds (design.md
        // § `dbg()`, § Contracts).
        std::env::set_var("KARAC_STRIP_DBG", "1");
        std::env::set_var("KARAC_STRIP_CONTRACTS", "1");
    }
    let source = match &project {
        Some(p) => p.text.clone(),
        None => std::fs::read_to_string(filename).unwrap_or_else(|e| {
            fail_with(&format!("error: {filename}: {e}"), output);
            process::exit(1);
        }),
    };
    super::run_v2::check_v2(
        filename,
        &source,
        project.as_ref(),
        manifest,
        a.lint_overrides,
        output,
    );
    crate::mir::interp::set_source_name(filename);
    if let Some(p) = &project {
        p.install();
    }
    #[cfg(feature = "llvm")]
    {
        let fail = |msg: String| -> ! {
            fail_with(&msg, output);
            process::exit(1);
        };
        let opt = match std::env::var("KARAC_MIR_OPT") {
            Ok(v) => v
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= 3)
                .unwrap_or_else(|| fail(format!("error: KARAC_MIR_OPT={v}: expected 0 to 3"))),
            Err(_) => 2,
        };
        let stem = std::path::Path::new(exe)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("main");
        let obj = std::env::temp_dir().join(format!("karac_mir_{}_{stem}.o", process::id()));
        let keep_ll = std::env::var_os("KARAC_MIR_LL").map(std::path::PathBuf::from);
        let lowered = crate::mir::lower::build_source(&source)
            .and_then(|l| {
                // Conflicting `par` branches and invalid MIR are refused,
                // as on every MIR backend.
                crate::mir::lower::compiled_check(&l)?;
                Ok(l)
            })
            .unwrap_or_else(|e| super::run_v2::refused(&e, output));
        let built = crate::backend::mir_aot::compile_program_object(
            &lowered.program,
            &lowered.tys,
            &obj,
            opt,
            keep_ll.as_deref(),
        )
        .map_err(|e| format!("llvm: {e}"))
        .and_then(|()| {
            let mut t = std::time::Instant::now();
            let r = crate::backend::driver::link_executable(&obj.to_string_lossy(), exe);
            crate::mir::lower::phase_time("link", &mut t);
            r
        });
        let _ = std::fs::remove_file(&obj);
        if let Err(e) = built {
            fail(format!("error[mir]: {e}"));
        }
        match output {
            OutputMode::Text => println!("Built: {exe}"),
            _ => println!("{{\"status\":\"ok\",\"output\":{}}}", json_string(exe)),
        }
    }
    // Without the llvm feature there is no code generator: the check above
    // is the whole build, as on the legacy path.
    #[cfg(not(feature = "llvm"))]
    eprintln!(
        "note: karac build requires the llvm feature; {filename} type-checked but no \
         executable was produced (wanted at {exe})."
    );
}

/// A build error: the message as written in text mode (it names its own
/// `error:` prefix), the build-error object under `--output=json`/`jsonl`.
fn fail_with(msg: &str, output: OutputMode) {
    match output {
        OutputMode::Text => eprintln!("{msg}"),
        _ => emit_build_error(msg, output),
    }
}
